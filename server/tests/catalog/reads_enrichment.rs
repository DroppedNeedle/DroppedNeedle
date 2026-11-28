//! Search-result counts run through the app against a scripted
//! ListenBrainz and the recorded Last.fm answer: a failed row stays on its
//! row.

/// ListenBrainz on a loopback origin, taking any token and refusing the
/// second artist, and Last.fm answering every read with the recorded
/// `artist.getInfo`.
fn counts_stub(artist_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> axum::Router {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use std::sync::atomic::Ordering;
    axum::Router::new()
        .route(
            "/lastfm/",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    include_str!("../fixtures/lastfm/artist_getinfo.json"),
                )
            }),
        )
        .route(
            "/listenbrainz/1/popularity/artist",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let artist_calls = std::sync::Arc::clone(&artist_calls);
                async move {
                    artist_calls.fetch_add(1, Ordering::SeqCst);
                    let ids: Vec<String> = body["artist_mbids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    if ids.iter().any(|id| id == "artist-mbid-2") {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                    let rows: Vec<serde_json::Value> = ids
                        .into_iter()
                        .filter_map(|id| match id.as_str() {
                            "artist-mbid-1" => Some(serde_json::json!({
                                "artist_mbid": id,
                                "total_listen_count": 4_000_000,
                                "total_user_count": 200_000,
                            })),
                            "artist-mbid-3" => Some(serde_json::json!({
                                "artist_mbid": id,
                                "total_listen_count": 5_000,
                                "total_user_count": 500,
                            })),
                            _ => None,
                        })
                        .collect();
                    axum::Json(serde_json::Value::Array(rows)).into_response()
                }
            }),
        )
        .route(
            "/listenbrainz/1/validate-token",
            get(|| async { axum::Json(serde_json::json!({"valid": true, "user_name": "owner"})) }),
        )
        .route(
            "/listenbrainz/1/popularity/release-group",
            post(|| async {
                axum::Json(serde_json::json!([
                    {"release_group_mbid": "rg-mbid-1", "total_listen_count": 90_000},
                ]))
            }),
        )
}

/// ListenBrainz counts read artist counts in one anonymous batch call and
/// album counts in one batch call; a refused artist batch leaves those rows
/// without counts and notes ListenBrainz once while album counts still land.
/// With ListenBrainz off, an instance Last.fm key gives Last.fm counts.
#[tokio::test]
async fn a_failed_count_stays_on_its_row() {
    let artist_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn({
        let artist_calls = std::sync::Arc::clone(&artist_calls);
        async move {
            axum::serve(listener, counts_stub(artist_calls))
                .await
                .unwrap()
        }
    });
    let app =
        crate::common::TestApp::start_with("search-counts", crate::common::outbound_at(&origin))
            .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/me/connections/listenbrainz",
            &auth,
            Some(serde_json::json!({"username": "owner", "user_token": "owner-token"})),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &auth,
            Some(serde_json::json!({"reads_enabled": false})),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");

    // With neither service on, the ids echo back with no counts.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(serde_json::json!({
                "artists": [{"musicbrainz_id": "artist-mbid-1", "name": "Portishead"}],
                "albums": [],
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(body["source"].is_null(), "{body}");
    assert!(body["artists"][0]["listen_count"].is_null(), "{body}");
    assert_eq!(body["degradations"], serde_json::json!([]), "{body}");

    // With ListenBrainz off and an instance Last.fm key, Last.fm counts.
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/lastfm",
            &auth,
            Some(serde_json::json!({
                "enabled": true,
                "api_key": "instance-key",
                "shared_secret": "instance-secret",
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(serde_json::json!({
                "artists": [{"musicbrainz_id": "artist-mbid-1", "name": "Portishead"}],
                "albums": [],
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body["source"], "lastfm", "{body}");
    assert_eq!(body["artists"][0]["listen_count"], 8_489_583, "{body}");

    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &auth,
            Some(serde_json::json!({"reads_enabled": true})),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(serde_json::json!({
                "artists": [
                    {"musicbrainz_id": "artist-mbid-1", "name": "Portishead"},
                    {"musicbrainz_id": "artist-mbid-2", "name": "Massive Attack"},
                ],
                "albums": [
                    {"musicbrainz_id": "rg-mbid-1", "artist_name": "Portishead", "album_name": "Dummy"},
                ],
            })),
        )
        .await;

    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body["source"], "listenbrainz", "{body}");
    assert!(body["artists"][0]["listen_count"].is_null(), "{body}");
    assert!(body["artists"][1]["listen_count"].is_null(), "{body}");
    assert_eq!(body["albums"][0]["listen_count"], 90_000, "{body}");
    let notes = body["degradations"].as_array().expect("degradations");
    assert_eq!(notes.len(), 1, "{body}");
    assert_eq!(notes[0]["source"], "listenbrainz", "{body}");
    assert_eq!(
        artist_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "two search artists must go out in one batch POST /1/popularity/artist call"
    );
    app.stop().await;
    server.abort();
}

/// Multiple artists resolve in a single anonymous batch POST /1/popularity/artist
/// call, including artists known only by singles.
#[tokio::test]
async fn a_singles_only_artist_still_has_a_count() {
    let artist_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn({
        let artist_calls = std::sync::Arc::clone(&artist_calls);
        async move {
            axum::serve(listener, counts_stub(artist_calls))
                .await
                .unwrap()
        }
    });
    let app =
        crate::common::TestApp::start_with("search-singles", crate::common::outbound_at(&origin))
            .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(serde_json::json!({
                "artists": [
                    {"musicbrainz_id": "artist-mbid-1", "name": "Portishead"},
                    {"musicbrainz_id": "artist-mbid-3", "name": "Singles"},
                ],
                "albums": [],
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body["artists"][0]["listen_count"], 4_000_000, "{body}");
    assert_eq!(body["artists"][1]["listen_count"], 5_000, "{body}");
    assert_eq!(
        artist_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "two artists must be fetched in one batch call without requiring a user token"
    );
    app.stop().await;
    server.abort();
}

/// Library Management's AcoustID lookups reuse an answer: the same print
/// asked twice goes out once, and a lookup that met a failure keeps
/// nothing, so asking again goes out again.
#[tokio::test]
async fn acoustid_answers_are_reused_and_failures_are_not() {
    use droppedneedle_catalog::enrichment::library::LiveEnrichment;
    use droppedneedle_catalog::lyrics::OnlineLyrics;
    use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
    use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
    use droppedneedle_library::management::enrich::{
        AcoustIdAnswer, AcoustIdPrint, EnrichmentPort as _,
    };
    use droppedneedle_providers::acoustid::{self, Unanswered};
    use droppedneedle_providers::executor::Method;
    use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
    use droppedneedle_providers::{ProviderEndpoints, Providers};
    use std::sync::Arc;

    let rig = crate::common::auth::AuthRig::new("enrich-acoustid").await;
    rig.config
        .save_secret_blocking(TypedLibrary {
            acoustid_api_key: droppedneedle_kernel::runtime_config::Secret::new("key"),
            ..TypedLibrary::default()
        })
        .expect("library settings save");
    let wire = Arc::new(ScriptedWire::new());
    let lookup = format!("{}/v2/lookup", acoustid::DEFAULT_BASE_URL);
    let answer = |status: u16, body: serde_json::Value| Scripted::Answer {
        status,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: body.to_string().into_bytes(),
    };
    let heard = serde_json::json!({"status": "ok", "fingerprints": [
        {"index": 0, "results": [{"id": "a2558b28-8f9d-44ca-b97b-f800ff935a1b", "score": 0.9,
            "recordings": [{"id": "fe000616-c72c-43d4-8cba-a4083e99636b"}]}]}
    ]});
    let refused = serde_json::json!({
        "status": "error",
        "error": {"code": 3, "message": "invalid fingerprint"}
    });
    wire.on(Method::Post, &lookup, answer(200, heard.clone()));
    wire.on(Method::Post, &lookup, answer(400, refused));
    wire.on(Method::Post, &lookup, answer(200, heard));
    let providers = Arc::new(Providers::unpaced_at(
        wire.clone(),
        ProviderEndpoints {
            acoustid: acoustid::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let enrichment = LiveEnrichment::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        droppedneedle_catalog::test_support::artwork(&rig.platform, &providers),
        OnlineLyrics::new(&providers, rig.config.live()),
        providers.acoustid.clone(),
        rig.config.live(),
    );
    let print = |print: &str| AcoustIdPrint {
        print: print.to_owned(),
        duration_seconds: 218,
    };
    let ids = |answer: &AcoustIdAnswer| match answer {
        AcoustIdAnswer::Answered(results) => results
            .iter()
            .map(|results| results.iter().map(|result| result.id.clone()).collect())
            .collect::<Vec<Vec<String>>>(),
        other => panic!("expected an answer, got {other:?}"),
    };

    let first = enrichment.acoustid_lookup(&[print("one")], true).await;
    let again = enrichment.acoustid_lookup(&[print("one")], false).await;
    assert_eq!(ids(&first), ids(&again));
    assert_eq!(ids(&first)[0].len(), 1);
    assert_eq!(wire.sent().len(), 1, "the same print is asked once");

    // Beside the kept print only the new one goes out, and the error it
    // meets keeps nothing.
    let failed = enrichment
        .acoustid_lookup(&[print("one"), print("two")], true)
        .await;
    assert_eq!(failed, AcoustIdAnswer::Unavailable(Unanswered::Code(3)));
    assert_eq!(wire.sent().len(), 2);
    let retried = enrichment.acoustid_lookup(&[print("two")], true).await;
    assert_eq!(ids(&retried)[0].len(), 1);
    assert_eq!(wire.sent().len(), 3, "a failure then a retry make two");

    // A print AcoustID leaves out of its answer is not a "no match".
    let partial = enrichment
        .acoustid_lookup(&[print("three"), print("four")], true)
        .await;
    assert_eq!(partial, AcoustIdAnswer::Unavailable(Unanswered::Failed));
}

/// A genre source the admin switched off answers "off" and is never
/// asked; switched on, one that cannot be reached is a failure, never
/// "off", so the plan keeps the file's genres.
#[tokio::test]
async fn a_switched_off_genre_source_is_off_and_an_unreachable_one_fails() {
    use droppedneedle_catalog::enrichment::library::LiveEnrichment;
    use droppedneedle_catalog::lyrics::OnlineLyrics;
    use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
    use droppedneedle_kernel::runtime_config::sections::ListenBrainzSettings;
    use droppedneedle_library::management::enrich::{EnrichmentPort as _, GenreAnswer};
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, Providers};
    use std::sync::Arc;

    let rig = crate::common::auth::AuthRig::new("enrich-genres-off").await;
    let wire = Arc::new(ScriptedWire::new());
    let providers = Arc::new(Providers::unpaced_at(
        wire.clone(),
        ProviderEndpoints::closed(),
    ));
    let enrichment = LiveEnrichment::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        droppedneedle_catalog::test_support::artwork(&rig.platform, &providers),
        OnlineLyrics::new(&providers, rig.config.live()),
        providers.acoustid.clone(),
        rig.config.live(),
    );

    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: false,
        })
        .expect("ListenBrainz off");
    let group = "f5093c06-23e3-404f-aeaa-40f72885ee3a";
    assert_eq!(
        enrichment.listenbrainz_genres(group, false).await,
        Ok(GenreAnswer::Off)
    );
    assert_eq!(
        enrichment.lastfm_genres("Artist", "Album", false).await,
        Ok(GenreAnswer::Off)
    );
    assert!(wire.sent().is_empty(), "nothing was asked");

    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: true,
        })
        .expect("ListenBrainz on");
    assert!(
        enrichment.listenbrainz_genres(group, false).await.is_err(),
        "a closed address is a failure"
    );
}

#[tokio::test]
async fn lastfm_search_enrichment_uses_cache_on_repeat_calls() {
    use axum::routing::get;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let stub = axum::Router::new().route(
        "/lastfm/",
        get(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    include_str!("../fixtures/lastfm/artist_getinfo.json"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let app = crate::common::TestApp::start_with(
        "search-counts-lastfm-cache",
        crate::common::outbound_at(&origin),
    )
    .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, _) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &auth,
            Some(serde_json::json!({"reads_enabled": false})),
        )
        .await;
    assert!(status.is_success());
    let (status, _) = app
        .call(
            "PUT",
            "/api/v3/settings/lastfm",
            &auth,
            Some(serde_json::json!({
                "enabled": true,
                "api_key": "instance-key",
                "shared_secret": "instance-secret",
            })),
        )
        .await;
    assert!(status.is_success());

    let payload = serde_json::json!({
        "artists": [{"musicbrainz_id": "artist-mbid-cache-1", "name": "Portishead"}],
        "albums": [],
    });
    let (s1, b1) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(payload.clone()),
        )
        .await;
    assert!(s1.is_success(), "{s1} {b1}");
    assert_eq!(b1["artists"][0]["listen_count"], 8_489_583);
    // Give the async cache writer a brief beat to commit before the repeat call.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (s2, b2) = app
        .call("POST", "/api/v3/search/enrich/batch", &auth, Some(payload))
        .await;
    assert!(s2.is_success(), "{s2} {b2}");
    assert_eq!(b2["artists"][0]["listen_count"], 8_489_583);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "repeat Last.fm search enrichment must hit ProviderCacheStore"
    );
    server.abort();
}
