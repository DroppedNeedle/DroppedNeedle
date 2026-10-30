//! Search-result counts run through the app against a scripted
//! ListenBrainz: a failed row stays on its row.

/// ListenBrainz on a loopback origin, refusing the second artist.
fn listenbrainz_stub() -> axum::Router {
    use axum::extract::Path;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/listenbrainz/1/popularity/top-release-groups-for-artist/{mbid}",
            get(|Path(mbid): Path<String>| async move {
                if mbid == "artist-mbid-2" {
                    return StatusCode::FORBIDDEN.into_response();
                }
                axum::Json(serde_json::json!([
                    {"release_group_mbid": "rg-a", "total_listen_count": 3_000_000},
                    {"release_group_mbid": "rg-b", "total_listen_count": 1_000_000},
                ]))
                .into_response()
            }),
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

/// ListenBrainz counts sum an artist's top release groups and read album
/// counts in one call; a refused artist call leaves that row without a
/// count and notes ListenBrainz once, and the other rows still count.
#[tokio::test]
async fn a_failed_count_stays_on_its_row() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server =
        tokio::spawn(async move { axum::serve(listener, listenbrainz_stub()).await.unwrap() });
    let app =
        crate::common::TestApp::start_with("search-counts", crate::common::outbound_at(&origin))
            .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &auth,
            Some(serde_json::json!({"enabled": true, "username": "", "user_token": ""})),
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
    assert_eq!(body["artists"][0]["listen_count"], 4_000_000, "{body}");
    assert!(body["artists"][1]["listen_count"].is_null(), "{body}");
    assert_eq!(body["albums"][0]["listen_count"], 90_000, "{body}");
    let notes = body["degradations"].as_array().expect("degradations");
    assert_eq!(notes.len(), 1, "{body}");
    assert_eq!(notes[0]["source"], "listenbrainz", "{body}");
    app.stop().await;
    server.abort();
}

/// Library Management's AcoustID lookups reuse an answer: the same print
/// asked twice goes out once, and a lookup that met a failure keeps
/// nothing, so asking again goes out again.
#[tokio::test]
async fn acoustid_answers_are_reused_and_failures_are_not() {
    use droppedneedle_catalog::reads::catalog::upstream::{LiveCatalogSettings, Upstream};
    use droppedneedle_catalog::reads::enrichment::library::LiveEnrichment;
    use droppedneedle_catalog::reads::lyrics::OnlineLyrics;
    use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
    use droppedneedle_library::management::enrich::{
        AcoustIdAnswer, AcoustIdPrint, EnrichmentPort as _,
    };
    use droppedneedle_providers::acoustid::{self, Unanswered};
    use droppedneedle_providers::executor::{Method, Scripted, ScriptedWire};
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
        ),
        None,
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
