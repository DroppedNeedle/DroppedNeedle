//! Concerts end to end: a sweep against scripted Ticketmaster and Skiddle
//! fakes fills the feed, the routes narrow it to the user's city, the seen
//! marker clears the badge, a second sweep keeps first-seen times and
//! deletes what vanished upstream, and a sweep while Ticketmaster's daily
//! allowance is spent fails the artist without touching its listings.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::concerts::{ConcertsEvents, ConcertsNew, ConcertsSetup};
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::runtime_config::secret_sections::EventsSettings;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, Secret};
use droppedneedle_providers::{ProviderEndpoints, Providers, ReqwestWire};

use crate::common::ScratchDir;
use crate::common::auth::{test_principal, with_test_principal};

const MBID: &str = "AAAAAAAA-1111-2222-3333-444444444444";

fn day(offset: i64) -> String {
    (time::OffsetDateTime::now_utc().date() + time::Duration::days(offset)).to_string()
}

fn tm_event(id: &str, date: &str, venue: &str, city: &str, lat: &str, lon: &str) -> Value {
    json!({
        "id": id,
        "name": format!("The Band at {venue}"),
        "url": format!("https://tm.example/{id}"),
        "dates": {"start": {"localDate": date, "dateTime": format!("{date}T19:30:00Z")},
                  "status": {"code": "onsale"}},
        "_embedded": {"venues": [{
            "name": venue, "city": {"name": city}, "country": {"countryCode": "GB"},
            "location": {"latitude": lat, "longitude": lon}
        }]}
    })
}

/// Ticketmaster under `/tm`, Skiddle under `/sk`. `drop_o2` removes the
/// Liverpool gig from Ticketmaster for the second sweep; `spent` makes
/// Ticketmaster answer that the day's allowance is used up, resetting in
/// two minutes. `event_calls` counts what reached the events route.
fn fakes(
    attraction_calls: Arc<AtomicUsize>,
    drop_o2: Arc<AtomicBool>,
    spent: Arc<AtomicBool>,
    event_calls: Arc<AtomicUsize>,
) -> Router {
    Router::new()
        .route(
            "/tm/attractions.json",
            get(move || {
                attraction_calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    axum::Json(json!({"_embedded": {"attractions": [
                        {"id": "K-dj", "name": "The Band DJ Set"},
                        {"id": "K-real", "name": "The Band",
                         "externalLinks": {"musicbrainz": [{"id": MBID.to_lowercase()}]}}
                    ]}}))
                }
            }),
        )
        .route(
            "/tm/events.json",
            get(move || {
                event_calls.fetch_add(1, Ordering::SeqCst);
                if spent.load(Ordering::SeqCst) {
                    let reset_ms = (time::OffsetDateTime::now_utc().unix_timestamp() + 120) * 1000;
                    let answer = (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("rate-limit-reset", reset_ms.to_string())],
                        r#"{"fault": {"faultstring": "Rate limit quota violation. Quota limit exceeded. Identifier : tm-key", "detail": {"errorcode": "policies.ratelimit.QuotaViolation"}}}"#,
                    )
                        .into_response();
                    return std::future::ready(answer);
                }
                let mut events = vec![tm_event(
                    "tm-tokyo",
                    &day(20),
                    "Budokan",
                    "Tokyo",
                    "35.69",
                    "139.75",
                )];
                if !drop_o2.load(Ordering::SeqCst) {
                    events.push(tm_event(
                        "tm-o2",
                        &day(10),
                        "O2 Academy Liverpool",
                        "Liverpool",
                        "53.4001",
                        "-2.9801",
                    ));
                }
                // Past the one-year horizon: never stored.
                events.push(tm_event(
                    "tm-far",
                    &day(400),
                    "Olympia",
                    "Liverpool",
                    "53.40",
                    "-2.98",
                ));
                std::future::ready(
                    axum::Json(json!({"_embedded": {"events": events}, "page": {"totalPages": 1}}))
                        .into_response(),
                )
            }),
        )
        .route(
            "/sk/artists/",
            get(|| async {
                axum::Json(json!({"error": 0, "totalcount": 2, "results": [
                    {"id": "55", "name": "The Band"},
                    {"id": "56", "name": "The Bland"}
                ]}))
            }),
        )
        .route(
            "/sk/events/search/",
            get(|| async {
                axum::Json(json!({"error": 0, "totalcount": "2", "results": [
                    // Same gig as Ticketmaster's: dropped by the dedupe.
                    {"id": "sk-o2", "eventname": "The Band", "date": day(10),
                     "cancelled": "0", "rescheduledDate": "", "ticketUrl": "",
                     "link": "https://sk.example/o2",
                     "venue": {"name": "O2 Academy Liverpool", "town": "Liverpool",
                               "country": "GB", "latitude": 53.4, "longitude": -2.98}},
                    // No coordinates: matches the city by name.
                    {"id": "sk-cavern", "eventname": "The Band (acoustic)", "date": day(5),
                     "cancelled": "1", "rescheduledDate": "", "ticketUrl": " ",
                     "link": "https://sk.example/cavern",
                     "venue": {"name": "The Cavern", "town": "liverpool", "country": "GB"}}
                ]}))
            }),
        )
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, ConcertsNew)>>);

impl ConcertsEvents for Recorder {
    fn concerts_new(&self, user_id: &str, event: &ConcertsNew) {
        self.0
            .lock()
            .unwrap()
            .push((user_id.to_owned(), event.clone()));
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn sweep_fills_feed_and_routes_narrow_it_to_saved_cities() {
    let scratch = ScratchDir::new("concerts");
    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    let config = Arc::new(
        ConfigStore::scratch(&scratch, Crypto::from_key_bytes(&[7u8; 32]).unwrap()).unwrap(),
    );
    config
        .save_secret_blocking(EventsSettings {
            enabled: true,
            ticketmaster_enabled: true,
            ticketmaster_api_key: Secret::new("tm-key"),
            skiddle_enabled: true,
            skiddle_api_key: Secret::new("sk-key"),
            ..EventsSettings::default()
        })
        .unwrap();
    runtime
        .db()
        .write(Lane::Foreground, "seed follows", |tx| {
            for user in ["u1", "u2"] {
                tx.execute(
                    "INSERT INTO auth_users (id, display_name, created_at) VALUES (?1, ?1, 'now')",
                    [user],
                )?;
                tx.execute(
                    "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
                     artist_name, followed_at, updated_at) VALUES (?1, ?2, ?3, 'The Band', 1, 1)",
                    [user, MBID, &MBID.to_lowercase()],
                )?;
            }
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .unwrap();

    let attraction_calls = Arc::new(AtomicUsize::new(0));
    let drop_o2 = Arc::new(AtomicBool::new(false));
    let spent = Arc::new(AtomicBool::new(false));
    let event_calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = fakes(
        Arc::clone(&attraction_calls),
        Arc::clone(&drop_o2),
        Arc::clone(&spent),
        Arc::clone(&event_calls),
    );
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let recorder = Arc::new(Recorder::default());
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&http)),
        ProviderEndpoints {
            ticketmaster: format!("{base}/tm"),
            skiddle: format!("{base}/sk"),
            ..ProviderEndpoints::closed()
        },
    );
    let setup = ConcertsSetup::new(
        runtime.db().clone(),
        config.live(),
        &providers,
        recorder.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let app = with_test_principal(
        droppedneedle_catalog::concerts::handlers::routes()
            .with_state(setup.service().expect("wired concerts"))
            .into_router(),
        test_principal("u1", Role::User, "s1", SessionKind::Standard),
    );
    let stop = droppedneedle_kernel::jobs::CancellationToken::new();
    let sweep = setup.sweep().expect("wired concerts sweep").without_delay();

    let (_, first) = sweep.run(None, &stop).await.unwrap();
    assert_eq!(
        (first.artists_swept, first.events_new, first.errors),
        (1, 3, 0)
    );
    let notified: Vec<(String, usize)> = recorder
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(user, event)| (user.clone(), event.new_events))
        .collect();
    assert_eq!(notified, [("u1".to_owned(), 3), ("u2".to_owned(), 3)]);

    // No cities yet: nothing to show, nothing unseen.
    let (status, body) = call(&app, "GET", "/api/v3/following/concerts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], true);
    assert_eq!(body["total"], 0);

    let (status, body) = call(
        &app,
        "PUT",
        "/api/v3/following/concerts/cities",
        Some(json!({"items": [
            {"city_name": " Liverpool ", "latitude": 53.41, "longitude": -2.98, "country_code": "GB"},
            {"city_name": "", "latitude": 1.0, "longitude": 1.0}
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["items"],
        json!([{"city_name": "Liverpool", "latitude": 53.41, "longitude": -2.98,
                "radius_km": 30.0, "country_code": "GB"}])
    );

    let (status, body) = call(&app, "GET", "/api/v3/following/concerts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(body["total"], 2, "{body}");
    assert_eq!(items[0]["source_event_id"], "sk-cavern");
    assert_eq!(items[0]["status"], "cancelled");
    assert_eq!(items[0]["distance_km"], Value::Null);
    assert_eq!(items[0]["ticket_url"], "https://sk.example/cavern");
    assert_eq!(items[0]["matched_city"], "Liverpool");
    assert_eq!(items[1]["source_event_id"], "tm-o2");
    assert_eq!(items[1]["source"], "ticketmaster");
    assert_eq!(items[1]["artist_mbid"], MBID);
    assert_eq!(items[1]["distance_km"], 1.1);

    let (_, body) = call(&app, "GET", "/api/v3/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 2}));
    let (_, body) = call(&app, "POST", "/api/v3/following/concerts/seen", None).await;
    assert_eq!(body, json!({"count": 0}));
    let (_, body) = call(&app, "GET", "/api/v3/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 0}));

    // Second sweep: resolutions come from the cache and the vanished
    // Ticketmaster gig goes. Skiddle's copy of it is no longer a duplicate,
    // so it is the one new row; the surviving row keeps its first-seen time,
    // so the badge counts only the new one.
    drop_o2.store(true, Ordering::SeqCst);
    let (_, second) = sweep.run(None, &stop).await.unwrap();
    assert_eq!((second.events_new, second.errors), (1, 0));
    assert_eq!(attraction_calls.load(Ordering::SeqCst), 1);
    let (_, body) = call(&app, "GET", "/api/v3/following/concerts", None).await;
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source_event_id"].as_str())
        .collect();
    assert_eq!(ids, ["sk-cavern", "sk-o2"]);
    let (_, body) = call(&app, "GET", "/api/v3/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 1}));

    // Ticketmaster's daily allowance runs out, resetting in two minutes.
    // The sweep counts the artist as failed and keeps every listing it had;
    // the next sweep sends nothing while Ticketmaster rests.
    spent.store(true, Ordering::SeqCst);
    let asked = event_calls.load(Ordering::SeqCst);
    let (_, third) = sweep.run(None, &stop).await.unwrap();
    assert_eq!((third.events_new, third.errors), (0, 1));
    assert_eq!(event_calls.load(Ordering::SeqCst), asked + 1);
    let (_, fourth) = sweep.run(None, &stop).await.unwrap();
    assert_eq!(fourth.errors, 1);
    assert_eq!(event_calls.load(Ordering::SeqCst), asked + 1);
    let (_, body) = call(&app, "GET", "/api/v3/following/concerts", None).await;
    let kept: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source_event_id"].as_str())
        .collect();
    assert_eq!(kept, ["sk-cavern", "sk-o2"]);

    // Bad input stays a 400 in the shared envelope.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v3/following/concerts/city-search?q=x",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "INVALID_INPUT");
}
