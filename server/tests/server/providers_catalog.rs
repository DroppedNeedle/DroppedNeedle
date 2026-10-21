//! Discogs, iTunes, preview (Deezer and iTunes), LRCLIB and Wikidata
//! clients against the scripted provider wire: the decode of each real
//! wire shape, identity gates, absence, the exact-id artist picture match,
//! and each provider's own rate-limit defaults. What every other status becomes is pinned in `providers_core`.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_providers::executor::{Method, Scripted, ScriptedWire};
use droppedneedle_providers::{
    PayloadKind, ProviderEndpoints, ProviderError, Providers, RequestPriority, TransportKind,
};
use droppedneedle_providers::{discogs, itunes, lrclib};

const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

const DISCOGS: &str = "http://fake/discogs";
const ITUNES: &str = "http://fake/itunes/search";
const DEEZER: &str = "http://fake/deezer";
const LRCLIB: &str = "http://fake/lrclib/api/get";

/// The providers over `wire`, unpaced, with the catalog family on a fake
/// host the scripted wire answers for.
fn catalog_providers(wire: &Arc<ScriptedWire>) -> Providers {
    Providers::unpaced_at(
        wire.clone(),
        ProviderEndpoints {
            discogs: DISCOGS.to_owned(),
            itunes: ITUNES.to_owned(),
            deezer: DEEZER.to_owned(),
            lrclib: "http://fake/lrclib".to_owned(),
            wikidata: "http://fake/data".to_owned(),
            wikipedia: "http://fake/{lang}.wiki".to_owned(),
            commons: "http://fake/commons".to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
}

/// One scripted answer: status, headers, body.
type Answer<'a> = (u16, &'a [(&'a str, &'a str)], &'a str);

/// A wire answering GETs of `url` with each of `answers` in turn.
fn scripted(url: &str, answers: &[Answer<'_>]) -> Arc<ScriptedWire> {
    let wire = Arc::new(ScriptedWire::new());
    for (status, headers, body) in answers {
        wire.answer(url, *status, headers, body.as_bytes().to_vec());
    }
    wire
}

/// A wire that never gets an answer from `url`.
fn unreachable(url: &str) -> Arc<ScriptedWire> {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        url,
        Scripted::Transport(TransportKind::Connect),
    );
    wire
}

/// How long `row` rests after the last call.
fn resting(providers: &Providers, row: &str) -> Duration {
    providers.limiter(row).expect("rate row").cooling_for()
}

// ---------------------------------------------------------------------------
// Discogs
// ---------------------------------------------------------------------------

/// Verified against api.discogs.com/releases/{id} on 2026-10-21: tests/fixtures/discogs/release_249504.json.
const DISCOGS_RELEASE: &str = include_str!("../fixtures/discogs/release_249504.json");
/// Verified against api.discogs.com/database/search on 2026-10-21: tests/fixtures/discogs/search_found.json.
const DISCOGS_SEARCH: &str = include_str!("../fixtures/discogs/search_found.json");
/// No results, recorded on 2026-10-21: tests/fixtures/discogs/search_no_results.json.
const DISCOGS_SEARCH_NO_RESULTS: &str = include_str!("../fixtures/discogs/search_no_results.json");

fn release_url(id: &str) -> String {
    format!("{DISCOGS}/releases/{id}")
}

#[tokio::test]
async fn discogs_contract_tolerates_unknown_fields() {
    let wire = scripted(&release_url("249504"), &[(200, &[], DISCOGS_RELEASE)]);
    let providers = catalog_providers(&wire);
    let release = providers
        .discogs
        .get_release("249504", 1.0, PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(release.release_id, "249504");
    assert_eq!(release.master_id.as_deref(), Some("96559"));
    assert_eq!(release.barcode.as_deref(), Some("5012394144777"));
    assert_eq!(release.media.len(), 1);
    assert_eq!(release.media[0].tracks[0].duration_seconds, Some(212.0));
    let debug = format!("{release:?}");
    assert!(!debug.contains("images"));
    assert!(!debug.contains("community"));
}

#[tokio::test]
async fn discogs_contract_incomplete_identity_is_unusable() {
    for body in [
        r#"{"id": 0, "title": "No Identity"}"#,
        r#"{"id": 5, "title": ""}"#,
    ] {
        let wire = scripted(&release_url("1"), &[(200, &[], body)]);
        let error = catalog_providers(&wire)
            .discogs
            .get_release("1", 1.0, PRIORITY)
            .await
            .expect_err(body);
        assert!(
            matches!(
                error,
                ProviderError::Payload {
                    kind: PayloadKind::Contract,
                    ..
                }
            ),
            "body: {body}: {error:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn discogs_degradation_429_keeps_retry_after() {
    let wire = scripted(&release_url("1"), &[(429, &[("retry-after", "7")], "{}")]);
    let error = catalog_providers(&wire)
        .discogs
        .get_release("1", 1.0, PRIORITY)
        .await
        .expect_err("429");
    assert!(
        matches!(
            error,
            ProviderError::RateLimited {
                retry_after: Some(wait),
                ..
            } if wait == Duration::from_secs(7)
        ),
        "{error:?}"
    );

    // No usable wait: Discogs rests a minute.
    for headers in [&[][..], &[("retry-after", "junk")][..]] {
        let wire = scripted(&release_url("1"), &[(429, headers, "{}")]);
        let providers = catalog_providers(&wire);
        let error = providers
            .discogs
            .get_release("1", 1.0, PRIORITY)
            .await
            .expect_err("429");
        assert!(matches!(error, ProviderError::RateLimited { .. }));
        assert!(resting(&providers, discogs::SOURCE) >= Duration::from_secs(59));
    }
}

#[tokio::test(start_paused = true)]
async fn discogs_degradation_500_transport_bad_json_unusable() {
    let wire = scripted(&release_url("1"), &[(500, &[], "{}")]);
    let error = catalog_providers(&wire)
        .discogs
        .get_release("1", 1.0, PRIORITY)
        .await
        .expect_err("500");
    assert!(matches!(error, ProviderError::Server { status: 500, .. }));

    let wire = unreachable(&release_url("1"));
    let error = catalog_providers(&wire)
        .discogs
        .get_release("1", 1.0, PRIORITY)
        .await
        .expect_err("transport");
    assert!(matches!(error, ProviderError::Transport { .. }));

    let wire = scripted(&release_url("1"), &[(200, &[], "not-json")]);
    let error = catalog_providers(&wire)
        .discogs
        .get_release("1", 1.0, PRIORITY)
        .await
        .expect_err("bad json");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        }
    ));

    let wire = scripted(&release_url("1"), &[(418, &[], "{}")]);
    let error = catalog_providers(&wire)
        .discogs
        .get_release("1", 1.0, PRIORITY)
        .await
        .expect_err("418");
    assert!(matches!(
        error,
        ProviderError::BadRequest { status: 418, .. }
    ));

    let wire = scripted(&release_url("1"), &[(404, &[], "{}")]);
    assert_eq!(
        catalog_providers(&wire)
            .discogs
            .get_release("1", 1.0, PRIORITY)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn discogs_contract_search_tolerates_unknown_fields() {
    let wire = scripted(
        &format!("{DISCOGS}/database/search"),
        &[(200, &[], DISCOGS_SEARCH)],
    );
    let candidates = catalog_providers(&wire)
        .discogs
        .search_releases("Nirvana Nevermind", 5, 1.0, PRIORITY)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 5);
    let first = &candidates[0];
    assert_eq!(first.release_id, "7097051");
    assert_eq!(first.master_id.as_deref(), Some("13814"));
    assert_eq!(first.artist_name, "Nirvana");
    assert_eq!(first.title, "Nevermind");
    assert_eq!(first.year, Some(2015));
    assert_eq!(first.label.as_deref(), Some("DGC"));
    assert_eq!(first.catalogue_number.as_deref(), Some("424 425-1"));
    assert_eq!(
        first.format_summary.as_deref(),
        Some("Vinyl, LP, Album, Reissue, Repress")
    );
    // The fifth hit has no year key.
    assert_eq!(candidates[4].year, None);
}

#[tokio::test]
async fn discogs_search_with_no_results_is_empty() {
    let wire = scripted(
        &format!("{DISCOGS}/database/search"),
        &[(200, &[], DISCOGS_SEARCH_NO_RESULTS)],
    );
    let candidates = catalog_providers(&wire)
        .discogs
        .search_releases("zxqvjkwpq", 5, 1.0, PRIORITY)
        .await
        .unwrap();
    assert!(candidates.is_empty());
}

#[tokio::test]
async fn discogs_search_hit_without_its_id_fails_the_page() {
    // The recorded page with the second hit's id taken out.
    let mut answer: serde_json::Value = serde_json::from_str(DISCOGS_SEARCH).unwrap();
    answer["results"][1]
        .as_object_mut()
        .expect("a second hit")
        .remove("id")
        .expect("the hit's id");
    let body = answer.to_string();
    let wire = scripted(
        &format!("{DISCOGS}/database/search"),
        &[(200, &[], body.as_str())],
    );
    let error = catalog_providers(&wire)
        .discogs
        .search_releases("Nirvana Nevermind", 5, 1.0, PRIORITY)
        .await
        .expect_err("a hit without its id");
    assert!(
        matches!(
            error,
            ProviderError::Payload {
                kind: PayloadKind::Decode,
                ..
            }
        ),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// iTunes
// ---------------------------------------------------------------------------

/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_album.json.
const ITUNES_ALBUMS: &str = include_str!("../fixtures/itunes/search_album.json");

#[tokio::test]
async fn itunes_contract_tolerates_unknown_fields() {
    // The recorded answer has many fields the client does not read. Its rows by
    // other artists, tribute albums included, fail the artist gate.
    let wire = scripted(ITUNES, &[(200, &[], ITUNES_ALBUMS)]);
    let found = catalog_providers(&wire)
        .itunes
        .find_album("Nirvana", "Nevermind", "US", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.artist_name, "Nirvana");
    assert!(found.url.starts_with("https://music.apple.com/us/album/"));
    let sent = wire.sent();
    assert_eq!(sent[0].query("term").as_deref(), Some("Nirvana Nevermind"));
    assert_eq!(sent[0].query("country").as_deref(), Some("US"));

    assert!(itunes::token_set_ratio("Nirvana", "Piano Tribute Players") < 80.0);
    assert!(itunes::token_set_ratio("Nirvana", "Nirvana") >= 80.0);
}

/// A 404 is absence and every other status keeps its own meaning; before,
/// any status but 200 and 429 read as an unusable answer.
#[tokio::test(start_paused = true)]
async fn itunes_degradation_429_500_bad_json() {
    let wire = scripted(ITUNES, &[(429, &[("retry-after", "9")], "{}")]);
    let error = catalog_providers(&wire)
        .itunes
        .find_album("A", "B", "US", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(
        error,
        ProviderError::RateLimited { retry_after: Some(wait), .. } if wait == Duration::from_secs(9)
    ));

    let wire = scripted(ITUNES, &[(429, &[], "{}")]);
    let providers = catalog_providers(&wire);
    let error = providers
        .itunes
        .find_album("A", "B", "US", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    assert!(resting(&providers, itunes::SOURCE) >= Duration::from_secs(59));

    let wire = scripted(ITUNES, &[(500, &[], "{}")]);
    let error = catalog_providers(&wire)
        .itunes
        .find_album("A", "B", "US", PRIORITY)
        .await
        .expect_err("500");
    assert!(matches!(error, ProviderError::Server { status: 500, .. }));

    let wire = scripted(ITUNES, &[(403, &[], "{}")]);
    let error = catalog_providers(&wire)
        .itunes
        .find_album("A", "B", "US", PRIORITY)
        .await
        .expect_err("403");
    assert!(matches!(error, ProviderError::Forbidden { .. }));

    let wire = scripted(ITUNES, &[(404, &[], "{}")]);
    assert_eq!(
        catalog_providers(&wire)
            .itunes
            .find_album("A", "B", "US", PRIORITY)
            .await
            .unwrap(),
        None
    );

    let wire = scripted(ITUNES, &[(200, &[], "<html>")]);
    let error = catalog_providers(&wire)
        .itunes
        .find_album("A", "B", "US", PRIORITY)
        .await
        .expect_err("bad json");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Preview (Deezer + iTunes)
// ---------------------------------------------------------------------------

fn deezer_search() -> String {
    format!("{DEEZER}/search")
}

/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_track.json.
const ITUNES_SONG: &str = include_str!("../fixtures/itunes/search_track.json");

#[tokio::test]
async fn preview_contract_tolerant_decode() {
    let body = include_str!("../fixtures/deezer/search_artist.json");
    let wire = scripted(&deezer_search(), &[(200, &[], body)]);
    let found = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found.title,
        "Boggling Queen (feat. Sister smurf & simple simon)"
    );
    assert_eq!(found.duration_s, Some(30));

    let wire = scripted(ITUNES, &[(200, &[], ITUNES_SONG)]);
    let found = catalog_providers(&wire)
        .previews
        .itunes_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.title, "Smells Like Teen Spirit");
    assert_eq!(found.duration_s, Some(30));
}

#[tokio::test]
async fn preview_error_object_inside_a_200() {
    // The recorded not-found answer, given for an album's track list, is absence.
    let album_search = format!("{DEEZER}/search/album");
    let album_tracks = format!("{DEEZER}/album/1007321681/tracks");
    let albums = include_str!("../fixtures/deezer/search_album.json");
    let wire = scripted(&album_search, &[(200, &[], albums)]);
    wire.answer(
        &album_tracks,
        200,
        &[],
        include_str!("../fixtures/deezer/album_not_found.json"),
    );
    let tracks = catalog_providers(&wire)
        .previews
        .deezer_album_tracks("Queen", "A Night At The Opera", 4, PRIORITY)
        .await
        .unwrap();
    assert!(tracks.is_empty());

    // Any other code is a refusal, never absence.
    let body = r#"{"error": {"type": "ParameterException",
        "message": "Wrong parameter", "code": 700}}"#;
    let wire = scripted(&deezer_search(), &[(200, &[], body)]);
    let error = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("an error object");
    assert!(matches!(
        error,
        ProviderError::BadRequest { status: 200, .. }
    ));
}

#[tokio::test]
async fn preview_track_without_preview_is_absence() {
    // The first hit's preview is null: the next hit with one wins.
    let body = r#"{"data": [
        {"id": 1, "title": "T", "preview": null, "artist": {"name": "A"}},
        {"id": 2, "title": "T", "preview": "https://cdn/t.mp3", "artist": {"name": "A"}}
    ]}"#;
    let wire = scripted(&deezer_search(), &[(200, &[], body)]);
    let found = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.preview_url, "https://cdn/t.mp3");
}

#[tokio::test(start_paused = true)]
async fn preview_quirk_track_fallback_chain() {
    // Deezer empty, iTunes hit.
    let no_hits = include_str!("../fixtures/deezer/search_track_no_hits.json");
    let wire = scripted(&deezer_search(), &[(200, &[], no_hits)]);
    wire.answer(ITUNES, 200, &[], ITUNES_SONG);
    let (found, provider) = catalog_providers(&wire)
        .previews
        .get_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await;
    assert_eq!(found.unwrap().title, "Smells Like Teen Spirit");
    assert_eq!(provider, Some("itunes"));

    // Deezer error still falls through to iTunes.
    let wire = unreachable(&deezer_search());
    wire.answer(ITUNES, 200, &[], ITUNES_SONG);
    let (found, provider) = catalog_providers(&wire)
        .previews
        .get_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await;
    assert!(found.is_some());
    assert_eq!(provider, Some("itunes"));

    // Both legs dead is quiet absence.
    let wire = unreachable(&deezer_search());
    wire.on(
        Method::Get,
        ITUNES,
        Scripted::Transport(TransportKind::Connect),
    );
    let (found, provider) = catalog_providers(&wire)
        .previews
        .get_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await;
    assert_eq!(found, None);
    assert_eq!(provider, None);
}

#[tokio::test(start_paused = true)]
async fn preview_degradation_leg_errors() {
    let wire = scripted(&deezer_search(), &[(429, &[("retry-after", "1")], "{}")]);
    let error = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(
        error,
        ProviderError::RateLimited { retry_after: Some(wait), .. } if wait == Duration::from_secs(1)
    ));

    // Without a wait, Deezer rests five seconds and iTunes a minute.
    let wire = scripted(&deezer_search(), &[(429, &[], "{}")]);
    let providers = catalog_providers(&wire);
    let error = providers
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    let rest = resting(&providers, "deezer");
    assert!(
        rest > Duration::from_secs(4) && rest <= Duration::from_secs(5),
        "{rest:?}"
    );

    let wire = scripted(ITUNES, &[(429, &[], "{}")]);
    let providers = catalog_providers(&wire);
    let error = providers
        .previews
        .itunes_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    assert!(resting(&providers, itunes::SOURCE) >= Duration::from_secs(59));

    let wire = scripted(&deezer_search(), &[(404, &[], "{}")]);
    assert_eq!(
        catalog_providers(&wire)
            .previews
            .deezer_track_preview("A", "T", PRIORITY)
            .await
            .unwrap(),
        None
    );

    let wire = scripted(ITUNES, &[(404, &[], "{}")]);
    assert!(
        catalog_providers(&wire)
            .previews
            .itunes_album_tracks("A", "C", 4, PRIORITY)
            .await
            .unwrap()
            .is_empty()
    );

    let wire = scripted(&deezer_search(), &[(500, &[], "{}")]);
    let error = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("500");
    assert!(matches!(error, ProviderError::Server { status: 500, .. }));

    let wire = scripted(ITUNES, &[(200, &[], "nope")]);
    let error = catalog_providers(&wire)
        .previews
        .itunes_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("bad json");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// LRCLIB
// ---------------------------------------------------------------------------

const LRCLIB_FOUND: &str = include_str!("../fixtures/lrclib/get_found.json");

async fn lyrics(wire: &Arc<ScriptedWire>) -> Result<lrclib::LyricsLookup, ProviderError> {
    catalog_providers(wire)
        .lrclib
        .get_exact_lyrics("T", "A", "C", 60, PRIORITY)
        .await
}

#[tokio::test]
async fn lrclib_contract_tolerates_unknown_fields() {
    let wire = scripted(LRCLIB, &[(200, &[], LRCLIB_FOUND)]);
    let found = catalog_providers(&wire)
        .lrclib
        .get_exact_lyrics(
            "Bohemian Rhapsody",
            "Queen",
            "A Night at the Opera",
            355,
            PRIORITY,
        )
        .await
        .unwrap();
    assert!(found.found);
    let candidate = found.candidate.unwrap();
    assert_eq!(candidate.provider_id, 19080);
    assert_eq!(candidate.track_name, "Bohemian Rhapsody");
    assert!(candidate.plain_lyrics.is_some());
    assert!(candidate.synced_lyrics.is_some());
    assert!(!candidate.instrumental);
    let sent = wire.sent();
    assert_eq!(
        sent[0].query("track_name").as_deref(),
        Some("Bohemian Rhapsody")
    );
    assert_eq!(sent[0].query("duration").as_deref(), Some("355"));
}

#[tokio::test]
async fn lrclib_contract_completeness_gate() {
    let base = r#"{"id": 42, "trackName": "T", "artistName": "A",
        "albumName": "C", "duration": 120.0, "instrumental": true}"#;
    let cases = [
        base.replace("42", "0"),
        base.replace("\"T\"", "\"  \""),
        base.replace("\"A\"", "\"\""),
        base.replace("\"C\"", "\"\""),
        base.replace("120.0", "0.0"),
    ];
    for body in cases {
        let wire = scripted(LRCLIB, &[(200, &[], &body)]);
        let error = lyrics(&wire).await.expect_err(&body);
        assert!(
            matches!(
                error,
                ProviderError::Payload {
                    kind: PayloadKind::Contract,
                    ..
                }
            ),
            "body: {body}: {error:?}"
        );
    }
}

#[tokio::test]
async fn lrclib_quirk_oversized_rejected() {
    let huge = "x".repeat(lrclib::MAX_LYRICS_BYTES + 1);
    let wire = scripted(LRCLIB, &[(200, &[], &huge)]);
    let error = lyrics(&wire).await.expect_err("oversized");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::TooLarge,
            ..
        }
    ));

    let big = "x".repeat(lrclib::MAX_LYRICS_CHARACTERS + 1);
    let body = format!(
        r#"{{"id": 1, "trackName": "T", "artistName": "A", "albumName": "C",
            "duration": 60.0, "instrumental": false, "plainLyrics": "{big}"}}"#
    );
    let wire = scripted(LRCLIB, &[(200, &[], &body)]);
    let error = lyrics(&wire).await.expect_err("oversized text");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Contract,
            ..
        }
    ));
}

/// A 404 is absence and every other status keeps its own meaning; before,
/// any 4xx but 404 and 429 read as an unusable answer.
#[tokio::test(start_paused = true)]
async fn lrclib_degradation_404_429_500_bad_json() {
    let not_found = include_str!("../fixtures/lrclib/get_not_found.json");
    let wire = scripted(LRCLIB, &[(404, &[], not_found)]);
    assert!(!lyrics(&wire).await.unwrap().found);

    // A wait past thirty seconds is held to thirty.
    let wire = scripted(LRCLIB, &[(429, &[("retry-after", "45")], "{}")]);
    let error = lyrics(&wire).await.expect_err("429");
    assert!(matches!(
        error,
        ProviderError::RateLimited { retry_after: Some(wait), .. } if wait == Duration::from_secs(30)
    ));

    // Without a wait, LRCLIB rests two seconds.
    let wire = scripted(LRCLIB, &[(429, &[], "{}")]);
    let providers = catalog_providers(&wire);
    let error = providers
        .lrclib
        .get_exact_lyrics("T", "A", "C", 60, PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    let rest = resting(&providers, lrclib::SOURCE);
    assert!(
        rest > Duration::from_secs(1) && rest <= Duration::from_secs(2),
        "{rest:?}"
    );

    let wire = scripted(LRCLIB, &[(500, &[], "{}")]);
    let error = lyrics(&wire).await.expect_err("500");
    assert!(matches!(error, ProviderError::Server { status: 500, .. }));

    let busy = include_str!("../fixtures/lrclib/get_busy.json");
    let wire = scripted(LRCLIB, &[(503, &[("retry-after", "1")], busy)]);
    let error = lyrics(&wire).await.expect_err("503");
    assert!(matches!(error, ProviderError::Unavailable { .. }));

    let wire = scripted(LRCLIB, &[(400, &[], "{}")]);
    let error = lyrics(&wire).await.expect_err("400");
    assert!(matches!(
        error,
        ProviderError::BadRequest { status: 400, .. }
    ));

    let wire = scripted(LRCLIB, &[(200, &[], "nope")]);
    let error = lyrics(&wire).await.expect_err("bad json");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Wikidata
// ---------------------------------------------------------------------------

const ENTITY: &str = "http://fake/data/wiki/Special:EntityData/Q44190.json";
const EXTRACT: &str = "http://fake/en.wiki/w/api.php";
const CLAIMS: &str = "http://fake/data/w/api.php";
const COMMONS: &str = "http://fake/commons/w/api.php";

#[tokio::test]
async fn wikidata_recorded_entity_reads_its_title() {
    let wire = scripted(
        ENTITY,
        &[(
            200,
            &[],
            include_str!("../fixtures/wikidata/entity_found.json"),
        )],
    );
    let title = catalog_providers(&wire)
        .wikidata
        .wikipedia_title_from_wikidata("Q44190", "en", PRIORITY)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("Radiohead"));
}

#[tokio::test]
async fn wikidata_quirk_bio_hop() {
    // A Wikidata URL hops entity -> sitelink -> extract.
    let entity = include_str!("../fixtures/wikidata/entity_found.json");
    let page = include_str!("../fixtures/wikidata/extract_found.json");
    let wire = scripted(ENTITY, &[(200, &[], entity)]);
    wire.answer(EXTRACT, 200, &[], page);
    let found = catalog_providers(&wire)
        .wikidata
        .get_bio_extract("https://www.wikidata.org/wiki/Q44190", "en", PRIORITY)
        .await
        .unwrap();
    assert_eq!(
        found.as_deref(),
        Some("Placeholder intro text for the Radiohead article.")
    );
    let sent = wire.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].url.as_str(), ENTITY);
    assert!(sent[1].url.as_str().starts_with(EXTRACT));
    assert_eq!(sent[1].query("titles").as_deref(), Some("Radiohead"));

    // A Wikipedia URL skips the entity hop, and its title is sent decoded
    // (encoded once by the query, never twice).
    let wire = scripted(EXTRACT, &[(200, &[], page)]);
    let found = catalog_providers(&wire)
        .wikidata
        .get_bio_extract(
            "https://en.wikipedia.org/wiki/Sigur_R%C3%B3s",
            "en",
            PRIORITY,
        )
        .await
        .unwrap();
    assert!(found.is_some());
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].query("titles").as_deref(), Some("Sigur_Rós"));
}

/// A 404 is absence and every other status keeps its own meaning; before,
/// any status but 404 and 429 read as a transport failure.
#[tokio::test(start_paused = true)]
async fn wikidata_degradation_404_absent_500_transport_bad_json() {
    let extract = |wire: Arc<ScriptedWire>| async move {
        catalog_providers(&wire)
            .wikidata
            .wikipedia_extract("X", "en", PRIORITY)
            .await
    };
    let wire = scripted(EXTRACT, &[(404, &[], "{}")]);
    assert_eq!(extract(wire).await.unwrap(), None);

    let wire = scripted(EXTRACT, &[(500, &[], "{}")]);
    assert!(matches!(
        extract(wire).await,
        Err(ProviderError::Server { status: 500, .. })
    ));

    let wire = scripted(EXTRACT, &[(403, &[], "{}")]);
    assert!(matches!(
        extract(wire).await,
        Err(ProviderError::Forbidden { .. })
    ));

    let wire = scripted(EXTRACT, &[(200, &[], "nope")]);
    assert!(matches!(
        extract(wire).await,
        Err(ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        })
    ));

    let wire = unreachable(EXTRACT);
    assert!(matches!(
        extract(wire).await,
        Err(ProviderError::Transport { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn wikidata_degradation_429_honors_retry_after() {
    let wire = scripted(
        "http://fake/data/wiki/Special:EntityData/Q1.json",
        &[(429, &[("retry-after", "7")], "{}")],
    );
    let error = catalog_providers(&wire)
        .wikidata
        .wikipedia_title_from_wikidata("Q1", "en", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(
        error,
        ProviderError::RateLimited { retry_after: Some(wait), .. } if wait == Duration::from_secs(7)
    ));

    let wire = scripted(EXTRACT, &[(429, &[], "{}")]);
    let providers = catalog_providers(&wire);
    let error = providers
        .wikidata
        .wikipedia_extract("X", "en", PRIORITY)
        .await
        .expect_err("429");
    assert!(matches!(error, ProviderError::RateLimited { .. }));
    assert!(resting(&providers, "wikimedia-api") >= Duration::from_secs(59));
}

/// Recorded "no such" answers read as absence: a title with no page, a title
/// no page can carry, a property with no statement, an id that names no
/// entity, a merged item's claims, and a file with no image info.
#[tokio::test]
async fn wikidata_recorded_absence_answers_read_as_absent() {
    let extract = |wire: Arc<ScriptedWire>| async move {
        catalog_providers(&wire)
            .wikidata
            .wikipedia_extract("X", "en", PRIORITY)
            .await
    };
    let missing = include_str!("../fixtures/wikidata/extract_not_found.json");
    assert_eq!(
        extract(scripted(EXTRACT, &[(200, &[], missing)]))
            .await
            .unwrap(),
        None
    );
    let invalid = include_str!("../fixtures/wikidata/extract_invalid.json");
    assert_eq!(
        extract(scripted(EXTRACT, &[(200, &[], invalid)]))
            .await
            .unwrap(),
        None
    );

    let claim = |wire: Arc<ScriptedWire>| async move {
        catalog_providers(&wire)
            .wikidata
            .claim_string("Q44190", "P18", PRIORITY)
            .await
    };
    let no_statement = include_str!("../fixtures/wikidata/claims_no_statement.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], no_statement)]))
            .await
            .unwrap(),
        None
    );
    let no_entity = include_str!("../fixtures/wikidata/claims_no_such_entity.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], no_entity)]))
            .await
            .unwrap(),
        None
    );
    let invalid_id = include_str!("../fixtures/wikidata/claims_invalid_id.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], invalid_id)]))
            .await
            .unwrap(),
        None
    );
    let merged = include_str!("../fixtures/wikidata/claims_unresolved_redirect.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], merged)]))
            .await
            .unwrap(),
        None
    );

    let wire = scripted(
        CLAIMS,
        &[(
            200,
            &[],
            include_str!("../fixtures/wikidata/claims_p18_found.json"),
        )],
    );
    wire.answer(
        COMMONS,
        200,
        &[],
        include_str!("../fixtures/wikidata/commons_imageinfo_not_found.json"),
    );
    let image = catalog_providers(&wire)
        .wikidata
        .get_artist_image("Q44190", PRIORITY)
        .await
        .unwrap();
    assert_eq!(image, None);
}

/// An API error whose code does not name a missing entity is a payload error,
/// not absence.
#[tokio::test]
async fn wikidata_other_api_errors_are_payload_errors() {
    let wire = scripted(CLAIMS, &[(200, &[], r#"{"error": {"code": "maxlag"}}"#)]);
    let error = catalog_providers(&wire)
        .wikidata
        .claim_string("Q44190", "P18", PRIORITY)
        .await
        .expect_err("an API error is not an answer");
    assert!(matches!(
        error,
        ProviderError::Payload {
            kind: PayloadKind::Contract,
            ..
        }
    ));
}

/// A redirected id answers with its target under the target's own id, and the
/// target's title is the one read.
#[tokio::test]
async fn wikidata_redirected_id_reads_the_target_entity() {
    let wire = scripted(
        "http://fake/data/wiki/Special:EntityData/Q13165295.json",
        &[(
            200,
            &[],
            include_str!("../fixtures/wikidata/entity_redirect.json"),
        )],
    );
    let title = catalog_providers(&wire)
        .wikidata
        .wikipedia_title_from_wikidata("Q13165295", "en", PRIORITY)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("Universe"));
}

/// An answer with no entity for the id asked is a payload error unless it holds
/// exactly one entity, which is then the target of a redirect.
#[tokio::test]
async fn wikidata_no_single_entity_for_the_id_asked_is_a_payload_error() {
    for body in [
        r#"{"entities": {}}"#,
        r#"{"entities": {"Q1": {"id": "Q1"}, "Q2": {"id": "Q2"}}}"#,
    ] {
        let wire = scripted(ENTITY, &[(200, &[], body)]);
        let error = catalog_providers(&wire)
            .wikidata
            .wikipedia_title_from_wikidata("Q44190", "en", PRIORITY)
            .await
            .expect_err("no single entity for the id asked");
        assert!(matches!(
            error,
            ProviderError::Payload {
                kind: PayloadKind::Decode,
                ..
            }
        ));
    }
}

/// An artist's picture is found by exact id, never by name: only the one
/// item whose MusicBrainz artist id (P434) is the artist's matches, and only
/// a plain positive integer is taken as its Deezer artist id (P2722).
#[tokio::test]
async fn wikidata_artist_picture_matches_by_exact_id() {
    const NIRVANA: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    let hits = |total: u64, titles: &[&str]| {
        let search: Vec<_> = titles
            .iter()
            .map(|title| serde_json::json!({"ns": 0, "title": title, "pageid": 1}))
            .collect();
        serde_json::json!({"query": {"searchinfo": {"totalhits": total}, "search": search}})
            .to_string()
    };
    let searches = [
        (
            "one hit",
            include_str!("../fixtures/wikidata/search_p434.json").to_owned(),
            Some("Q11649"),
        ),
        (
            "no hit",
            include_str!("../fixtures/wikidata/search_p434_no_hits.json").to_owned(),
            None,
        ),
        ("two hits", hits(2, &["Q11649", "Q2"]), None),
        ("one hit of several", hits(3, &["Q11649"]), None),
        (
            "a hit that is not an item",
            hits(1, &["Property:P434"]),
            None,
        ),
    ];
    for (name, body, item) in searches {
        let wire = scripted(CLAIMS, &[(200, &[], body.as_str())]);
        let found = catalog_providers(&wire)
            .wikidata
            .item_for_musicbrainz_artist(NIRVANA, PRIORITY)
            .await
            .unwrap();
        assert_eq!(found.as_deref(), item, "{name}");
        assert_eq!(
            wire.sent()[0].query("srsearch").as_deref(),
            Some("haswbstatement:P434=5b11f4ce-a62d-471e-81fc-a69a8278c7da"),
            "{name}"
        );
    }

    let wire = scripted(CLAIMS, &[]);
    let by_name = catalog_providers(&wire)
        .wikidata
        .item_for_musicbrainz_artist("Nirvana", PRIORITY)
        .await
        .unwrap();
    assert_eq!(by_name, None);
    assert!(wire.sent().is_empty(), "a name is never searched");

    let claim = |value: serde_json::Value, rank: &str| serde_json::json!({"mainsnak": {"datavalue": {"value": value, "type": "string"}}, "rank": rank});
    let picture_ids = |body: String| async move {
        let wire = scripted(CLAIMS, &[(200, &[], body.as_str())]);
        catalog_providers(&wire)
            .wikidata
            .artist_picture_ids("Q11649", PRIORITY)
            .await
            .unwrap()
            .unwrap()
    };
    let deezer_ids = [
        ("415", Some(415)),
        ("0", None),
        ("-415", None),
        ("+415", None),
        (" 415", None),
        ("4.15", None),
        ("415a", None),
        ("", None),
        ("99999999999999999999999", None),
    ];
    for (value, id) in deezer_ids {
        let body = serde_json::json!({"claims": {"P2722": [claim(value.into(), "normal")]}});
        let ids = picture_ids(body.to_string()).await;
        assert_eq!(ids.deezer_artist, id, "P2722 {value:?}");
    }
    let body = serde_json::json!({"claims": {"P2722": [
        claim("1".into(), "deprecated"),
        claim(serde_json::json!({"id": "Q1"}), "normal"),
        claim("415".into(), "normal"),
    ]}});
    let ids = picture_ids(body.to_string()).await;
    assert_eq!(
        ids.deezer_artist,
        Some(415),
        "deprecated and non-text claims are skipped"
    );

    let wire = scripted(
        CLAIMS,
        &[(
            200,
            &[],
            include_str!("../fixtures/wikidata/claims_all.json"),
        )],
    );
    let ids = catalog_providers(&wire)
        .wikidata
        .artist_picture_ids("Q11649", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids.deezer_artist, Some(415));
    assert_eq!(ids.commons_file.as_deref(), Some("Nirvana around 1992.jpg"));
    assert_eq!(
        wire.sent()[0].query("property"),
        None,
        "one call reads both"
    );
}
