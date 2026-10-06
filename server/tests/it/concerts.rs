//! Concerts end to end: a sweep against scripted Ticketmaster and Skiddle
//! fakes fills the feed, the routes narrow it to the user's city, the seen
//! marker clears the badge, and a second sweep keeps first-seen times and
//! deletes what vanished upstream.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::concerts::{ConcertsEvents, ConcertsNew, ConcertsSetup, Endpoints};
use droppedneedle::db::{DbConfig, Lane, open_runtime};
use droppedneedle::runtime_config::secret_sections::EventsSettings;
use droppedneedle::runtime_config::{ConfigStore, Crypto, Secret};

use crate::common::ScratchDir;

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
/// Liverpool gig from Ticketmaster for the second sweep.
fn fakes(attraction_calls: Arc<AtomicUsize>, drop_o2: Arc<AtomicBool>) -> Router {
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
                async move {
                    axum::Json(json!({"_embedded": {"events": events}, "page": {"totalPages": 1}}))
                }
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
    let runtime = open_runtime(&DbConfig::new(&scratch.join("app.db")))
        .await
        .unwrap();
    let config = Arc::new(
        ConfigStore::open(
            &scratch.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).unwrap(),
        )
        .unwrap(),
    );
    config
        .save_secret(EventsSettings {
            enabled: true,
            ticketmaster_enabled: true,
            ticketmaster_api_key: Secret::new("tm-key"),
            skiddle_enabled: true,
            skiddle_api_key: Secret::new("sk-key"),
            ..EventsSettings::default()
        })
        .unwrap();
    runtime
        .lane()
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
            Ok(())
        })
        .await
        .unwrap();

    let attraction_calls = Arc::new(AtomicUsize::new(0));
    let drop_o2 = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fake = fakes(Arc::clone(&attraction_calls), Arc::clone(&drop_o2));
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let recorder = Arc::new(Recorder::default());
    let setup = ConcertsSetup::new(
        runtime.pool().clone(),
        runtime.lane().clone(),
        reqwest::Client::new(),
        Arc::clone(&config),
        Endpoints {
            ticketmaster: format!("{base}/tm"),
            skiddle: format!("{base}/sk"),
            geocoding: "http://127.0.0.1:9".to_owned(),
        },
    )
    .with_events(recorder.clone());
    let app = setup.gated_router().layer(axum::Extension(CurrentSession {
        user_id: "u1".to_owned(),
        session_id: "s1".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    }));
    let stop = tokio::sync::Notify::new();
    let sweep = setup.sweep().without_delay();

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
    let (status, body) = call(&app, "GET", "/following/concerts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configured"], true);
    assert_eq!(body["total"], 0);

    let (status, body) = call(
        &app,
        "PUT",
        "/following/concerts/cities",
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

    let (status, body) = call(&app, "GET", "/following/concerts", None).await;
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

    let (_, body) = call(&app, "GET", "/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 2}));
    let (_, body) = call(&app, "POST", "/following/concerts/seen", None).await;
    assert_eq!(body, json!({"count": 0}));
    let (_, body) = call(&app, "GET", "/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 0}));

    // Second sweep: resolutions come from the cache and the vanished
    // Ticketmaster gig goes. Skiddle's copy of it is no longer a duplicate,
    // so it is the one new row; the surviving row keeps its first-seen time,
    // so the badge counts only the new one.
    drop_o2.store(true, Ordering::SeqCst);
    let (_, second) = sweep.run(None, &stop).await.unwrap();
    assert_eq!((second.events_new, second.errors), (1, 0));
    assert_eq!(attraction_calls.load(Ordering::SeqCst), 1);
    let (_, body) = call(&app, "GET", "/following/concerts", None).await;
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["source_event_id"].as_str())
        .collect();
    assert_eq!(ids, ["sk-cavern", "sk-o2"]);
    let (_, body) = call(&app, "GET", "/following/concerts/unseen-count", None).await;
    assert_eq!(body, json!({"count": 1}));

    // Bad input stays a 400 in the shared envelope.
    let (status, body) = call(&app, "GET", "/following/concerts/city-search?q=x", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "INVALID_INPUT");
}
