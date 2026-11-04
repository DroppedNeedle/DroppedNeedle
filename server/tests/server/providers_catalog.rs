//! Discogs, iTunes, preview (Deezer and iTunes), LRCLIB and Wikidata
//! clients against the scripted provider wire: the decode of each real
//! wire shape, identity gates, absence, the exact-id artist picture match,
//! and each provider's own rate-limit defaults. What every other status becomes is pinned in `providers_core`.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_providers::executor::Method;
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
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

/// Verified against api.discogs.com/releases/{id} on 2026-10-21: tests/fixtures/discogs/release_249504.json.
const DISCOGS_RELEASE: &str = include_str!("../fixtures/discogs/release_249504.json");
/// Verified against api.discogs.com/database/search on 2026-10-21: tests/fixtures/discogs/search_found.json.
const DISCOGS_SEARCH: &str = include_str!("../fixtures/discogs/search_found.json");
/// No results, recorded on 2026-10-21: tests/fixtures/discogs/search_no_results.json.
const DISCOGS_SEARCH_NO_RESULTS: &str = include_str!("../fixtures/discogs/search_no_results.json");
/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_album.json.
const ITUNES_ALBUMS: &str = include_str!("../fixtures/itunes/search_album.json");
/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_track.json.
const ITUNES_SONG: &str = include_str!("../fixtures/itunes/search_track.json");
const LRCLIB_FOUND: &str = include_str!("../fixtures/lrclib/get_found.json");

const ENTITY: &str = "http://fake/data/wiki/Special:EntityData/Q44190.json";
const EXTRACT: &str = "http://fake/en.wiki/w/api.php";
const CLAIMS: &str = "http://fake/data/w/api.php";
const COMMONS: &str = "http://fake/commons/w/api.php";

fn release_url(id: &str) -> String {
    format!("{DISCOGS}/releases/{id}")
}

fn deezer_search() -> String {
    format!("{DEEZER}/search")
}

/// One catalog call, reduced to whether it found something.
#[derive(Clone, Copy, Debug)]
enum Ask {
    DiscogsRelease,
    ItunesAlbum,
    DeezerPreview,
    ItunesPreview,
    ItunesAlbumTracks,
    Lyrics,
    WikidataTitle,
    WikipediaExtract,
    WikidataClaim,
}

impl Ask {
    fn url(self) -> String {
        match self {
            Ask::DiscogsRelease => release_url("1"),
            Ask::ItunesAlbum | Ask::ItunesPreview | Ask::ItunesAlbumTracks => ITUNES.to_owned(),
            Ask::DeezerPreview => deezer_search(),
            Ask::Lyrics => LRCLIB.to_owned(),
            Ask::WikidataTitle => ENTITY.to_owned(),
            Ask::WikipediaExtract => EXTRACT.to_owned(),
            Ask::WikidataClaim => CLAIMS.to_owned(),
        }
    }

    async fn send(self, providers: &Providers) -> Result<bool, ProviderError> {
        match self {
            Ask::DiscogsRelease => providers
                .discogs
                .get_release("1", 1.0, PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::ItunesAlbum => providers
                .itunes
                .find_album("A", "B", "US", PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::DeezerPreview => providers
                .previews
                .deezer_track_preview("A", "T", PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::ItunesPreview => providers
                .previews
                .itunes_track_preview("A", "T", PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::ItunesAlbumTracks => providers
                .previews
                .itunes_album_tracks("A", "C", 4, PRIORITY)
                .await
                .map(|tracks| !tracks.is_empty()),
            Ask::Lyrics => providers
                .lrclib
                .get_exact_lyrics("T", "A", "C", 60, PRIORITY)
                .await
                .map(|lookup| lookup.found),
            Ask::WikidataTitle => providers
                .wikidata
                .wikipedia_title_from_wikidata("Q44190", "en", PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::WikipediaExtract => providers
                .wikidata
                .wikipedia_extract("X", "en", PRIORITY)
                .await
                .map(|found| found.is_some()),
            Ask::WikidataClaim => providers
                .wikidata
                .claim_string("Q44190", "P18", PRIORITY)
                .await
                .map(|found| found.is_some()),
        }
    }
}

/// What each recorded answer means once read: the values a page shows, and
/// absence where the provider says "no such".
#[tokio::test]
async fn recorded_catalog_answers_read_as_answered() {
    // Discogs release: unknown fields tolerated, values read.
    let wire = scripted(&release_url("249504"), &[(200, &[], DISCOGS_RELEASE)]);
    let release = catalog_providers(&wire)
        .discogs
        .get_release("249504", 1.0, PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(release.release_id, "249504", "discogs release");
    assert_eq!(
        release.master_id.as_deref(),
        Some("96559"),
        "discogs release"
    );
    assert_eq!(
        release.barcode.as_deref(),
        Some("5012394144777"),
        "discogs release"
    );
    assert_eq!(release.media.len(), 1, "discogs release");
    assert_eq!(
        release.media[0].tracks[0].duration_seconds,
        Some(212.0),
        "discogs release"
    );
    let debug = format!("{release:?}");
    assert!(!debug.contains("images"), "discogs release");
    assert!(!debug.contains("community"), "discogs release");

    // Discogs search.
    let wire = scripted(
        &format!("{DISCOGS}/database/search"),
        &[(200, &[], DISCOGS_SEARCH)],
    );
    let candidates = catalog_providers(&wire)
        .discogs
        .search_releases("Nirvana Nevermind", 5, 1.0, PRIORITY)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 5, "discogs search");
    let first = &candidates[0];
    assert_eq!(first.release_id, "7097051", "discogs search");
    assert_eq!(first.master_id.as_deref(), Some("13814"), "discogs search");
    assert_eq!(first.artist_name, "Nirvana", "discogs search");
    assert_eq!(first.title, "Nevermind", "discogs search");
    assert_eq!(first.year, Some(2015), "discogs search");
    assert_eq!(first.label.as_deref(), Some("DGC"), "discogs search");
    assert_eq!(
        first.catalogue_number.as_deref(),
        Some("424 425-1"),
        "discogs search"
    );
    assert_eq!(
        first.format_summary.as_deref(),
        Some("Vinyl, LP, Album, Reissue, Repress"),
        "discogs search"
    );
    assert_eq!(
        candidates[4].year, None,
        "discogs search: the fifth hit has no year key"
    );

    // Discogs search with no results.
    let wire = scripted(
        &format!("{DISCOGS}/database/search"),
        &[(200, &[], DISCOGS_SEARCH_NO_RESULTS)],
    );
    let candidates = catalog_providers(&wire)
        .discogs
        .search_releases("zxqvjkwpq", 5, 1.0, PRIORITY)
        .await
        .unwrap();
    assert!(candidates.is_empty(), "discogs search with no results");

    // Deezer and iTunes previews.
    let body = include_str!("../fixtures/deezer/search_artist.json");
    let wire = scripted(&deezer_search(), &[(200, &[], body)]);
    let found = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found.title, "Boggling Queen (feat. Sister smurf & simple simon)",
        "deezer preview"
    );
    assert_eq!(found.duration_s, Some(30), "deezer preview");

    let wire = scripted(ITUNES, &[(200, &[], ITUNES_SONG)]);
    let found = catalog_providers(&wire)
        .previews
        .itunes_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.title, "Smells Like Teen Spirit", "itunes preview");
    assert_eq!(found.duration_s, Some(30), "itunes preview");

    // LRCLIB.
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
    assert!(found.found, "lrclib");
    let candidate = found.candidate.unwrap();
    assert_eq!(candidate.provider_id, 19080, "lrclib");
    assert_eq!(candidate.track_name, "Bohemian Rhapsody", "lrclib");
    assert!(candidate.plain_lyrics.is_some(), "lrclib");
    assert!(candidate.synced_lyrics.is_some(), "lrclib");
    assert!(!candidate.instrumental, "lrclib");
    let sent = wire.sent();
    assert_eq!(
        sent[0].query("track_name").as_deref(),
        Some("Bohemian Rhapsody"),
        "lrclib"
    );
    assert_eq!(sent[0].query("duration").as_deref(), Some("355"), "lrclib");

    // Wikidata entity reads its title.
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
    assert_eq!(title.as_deref(), Some("Radiohead"), "wikidata entity");

    // A redirected id answers with its target under the target's own id, and
    // the target's title is the one read.
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
    assert_eq!(title.as_deref(), Some("Universe"), "wikidata redirected id");

    // Recorded "no such" answers read as absence: a title with no page, a
    // title no page can carry, a property with no statement, an id that names
    // no entity, a merged item's claims, and a file with no image info.
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
        None,
        "wikipedia extract not found"
    );
    let invalid = include_str!("../fixtures/wikidata/extract_invalid.json");
    assert_eq!(
        extract(scripted(EXTRACT, &[(200, &[], invalid)]))
            .await
            .unwrap(),
        None,
        "wikipedia extract invalid title"
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
        None,
        "wikidata claims, no statement"
    );
    let no_entity = include_str!("../fixtures/wikidata/claims_no_such_entity.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], no_entity)]))
            .await
            .unwrap(),
        None,
        "wikidata claims, no such entity"
    );
    let invalid_id = include_str!("../fixtures/wikidata/claims_invalid_id.json");
    assert_eq!(
        claim(scripted(CLAIMS, &[(200, &[], invalid_id)]))
            .await
            .unwrap(),
        None,
        "wikidata claims, invalid id"
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
    assert_eq!(image, None, "commons file without image info");

    // An artist whose link names a merged item still gets its picture: the
    // claims endpoint refuses the merged id, so it is resolved to its target
    // through Special:EntityData and the target's image is read.
    let wire = scripted(
        CLAIMS,
        &[
            (
                200,
                &[],
                include_str!("../fixtures/wikidata/claims_unresolved_redirect.json"),
            ),
            (
                200,
                &[],
                include_str!("../fixtures/wikidata/claims_redirect_target_p18.json"),
            ),
        ],
    );
    wire.answer(
        "http://fake/data/wiki/Special:EntityData/Q13165295.json",
        200,
        &[],
        include_str!("../fixtures/wikidata/entity_redirect.json"),
    );
    wire.answer(
        COMMONS,
        200,
        &[],
        include_str!("../fixtures/wikidata/commons_imageinfo_found.json"),
    );
    let image = catalog_providers(&wire)
        .wikidata
        .get_artist_image("Q13165295", PRIORITY)
        .await
        .unwrap();
    assert!(image.is_some(), "merged item");
    let sent = wire.sent();
    let claims: Vec<_> = sent
        .iter()
        .filter(|request| request.query("action").as_deref() == Some("wbgetclaims"))
        .map(|request| request.query("entity"))
        .collect();
    assert_eq!(
        claims,
        [Some("Q13165295".to_owned()), Some("Q1".to_owned())],
        "merged item"
    );
    assert_eq!(
        sent.last()
            .and_then(|request| request.query("titles"))
            .as_deref(),
        Some("File:NASA-HS201427a-HubbleUltraDeepField2014-20140603.jpg"),
        "merged item"
    );
}

/// An answer missing its identity, too large, or not naming the one entity
/// asked for is unusable, with the payload kind that says why.
#[tokio::test]
async fn catalog_answers_without_their_identity_are_unusable() {
    let lrclib_base = r#"{"id": 42, "trackName": "T", "artistName": "A",
        "albumName": "C", "duration": 120.0, "instrumental": true}"#;
    let long_text = "x".repeat(lrclib::MAX_LYRICS_CHARACTERS + 1);
    let rows: Vec<(&str, Ask, String, PayloadKind)> = vec![
        (
            "discogs release id 0",
            Ask::DiscogsRelease,
            r#"{"id": 0, "title": "No Identity"}"#.to_owned(),
            PayloadKind::Contract,
        ),
        (
            "discogs release without a title",
            Ask::DiscogsRelease,
            r#"{"id": 5, "title": ""}"#.to_owned(),
            PayloadKind::Contract,
        ),
        (
            "lrclib id 0",
            Ask::Lyrics,
            lrclib_base.replace("42", "0"),
            PayloadKind::Contract,
        ),
        (
            "lrclib blank track name",
            Ask::Lyrics,
            lrclib_base.replace("\"T\"", "\"  \""),
            PayloadKind::Contract,
        ),
        (
            "lrclib empty artist",
            Ask::Lyrics,
            lrclib_base.replace("\"A\"", "\"\""),
            PayloadKind::Contract,
        ),
        (
            "lrclib empty album",
            Ask::Lyrics,
            lrclib_base.replace("\"C\"", "\"\""),
            PayloadKind::Contract,
        ),
        (
            "lrclib zero duration",
            Ask::Lyrics,
            lrclib_base.replace("120.0", "0.0"),
            PayloadKind::Contract,
        ),
        (
            "lrclib body over the byte cap",
            Ask::Lyrics,
            "x".repeat(lrclib::MAX_LYRICS_BYTES + 1),
            PayloadKind::TooLarge,
        ),
        (
            "lrclib lyrics over the character cap",
            Ask::Lyrics,
            format!(
                r#"{{"id": 1, "trackName": "T", "artistName": "A", "albumName": "C",
            "duration": 60.0, "instrumental": false, "plainLyrics": "{long_text}"}}"#
            ),
            PayloadKind::Contract,
        ),
        (
            "wikidata answer with no entity",
            Ask::WikidataTitle,
            r#"{"entities": {}}"#.to_owned(),
            PayloadKind::Decode,
        ),
        (
            "wikidata answer with two entities, neither the one asked",
            Ask::WikidataTitle,
            r#"{"entities": {"Q1": {"id": "Q1"}, "Q2": {"id": "Q2"}}}"#.to_owned(),
            PayloadKind::Decode,
        ),
        (
            "wikidata API error that names no missing entity",
            Ask::WikidataClaim,
            r#"{"error": {"code": "maxlag"}}"#.to_owned(),
            PayloadKind::Contract,
        ),
    ];
    for (case, ask, body, kind) in rows {
        let wire = scripted(&ask.url(), &[(200, &[], body.as_str())]);
        let error = ask.send(&catalog_providers(&wire)).await.expect_err(case);
        assert!(
            matches!(&error, ProviderError::Payload { kind: got, .. } if *got == kind),
            "{case}: {error:?}"
        );
    }
}

/// How a scripted catalog call is answered.
enum Reply {
    Status(u16, &'static [(&'static str, &'static str)], &'static str),
    Unreachable,
}

/// The rest a rate row must be in after the call.
enum Rest {
    /// At least 59 seconds: a minute, less the time the call took.
    AMinute(&'static str),
    /// More than the first and at most the second number of seconds.
    Within(&'static str, u64, u64),
}

struct StatusRow {
    case: &'static str,
    ask: Ask,
    reply: Reply,
    expect: fn(&Result<bool, ProviderError>) -> bool,
    rest: Option<Rest>,
}

/// Each provider's 404 is absence and its own 429 rest default and cap hold;
/// every other status keeps its own meaning. Before, some providers read any
/// status but 200 and 429 (or 404) as an unusable answer or a transport failure.
#[tokio::test(start_paused = true)]
async fn catalog_statuses_keep_their_meaning() {
    let rows = [
        StatusRow {
            case: "discogs 429 with a wait",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(429, &[("retry-after", "7")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(7)),
            rest: None,
        },
        StatusRow {
            case: "discogs 429 without a wait rests a minute",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::AMinute(discogs::SOURCE)),
        },
        StatusRow {
            case: "discogs 429 with an unreadable wait rests a minute",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(429, &[("retry-after", "junk")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::AMinute(discogs::SOURCE)),
        },
        StatusRow {
            case: "discogs 500",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(500, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Server { status: 500, .. })),
            rest: None,
        },
        StatusRow {
            case: "discogs unreachable",
            ask: Ask::DiscogsRelease,
            reply: Reply::Unreachable,
            expect: |r| matches!(r, Err(ProviderError::Transport { .. })),
            rest: None,
        },
        StatusRow {
            case: "discogs bad json",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(200, &[], "not-json"),
            expect: |r| {
                matches!(
                    r,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::Decode,
                        ..
                    })
                )
            },
            rest: None,
        },
        StatusRow {
            case: "discogs 418",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(418, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::BadRequest { status: 418, .. })),
            rest: None,
        },
        StatusRow {
            case: "discogs 404 is absence",
            ask: Ask::DiscogsRelease,
            reply: Reply::Status(404, &[], "{}"),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "itunes 429 with a wait",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(429, &[("retry-after", "9")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(9)),
            rest: None,
        },
        StatusRow {
            case: "itunes 429 without a wait rests a minute",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::AMinute(itunes::SOURCE)),
        },
        StatusRow {
            case: "itunes 500",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(500, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Server { status: 500, .. })),
            rest: None,
        },
        StatusRow {
            case: "itunes 403",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(403, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Forbidden { .. })),
            rest: None,
        },
        StatusRow {
            case: "itunes 404 is absence",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(404, &[], "{}"),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "itunes bad json",
            ask: Ask::ItunesAlbum,
            reply: Reply::Status(200, &[], "<html>"),
            expect: |r| {
                matches!(
                    r,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::Decode,
                        ..
                    })
                )
            },
            rest: None,
        },
        StatusRow {
            case: "deezer preview 429 with a wait",
            ask: Ask::DeezerPreview,
            reply: Reply::Status(429, &[("retry-after", "1")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(1)),
            rest: None,
        },
        StatusRow {
            case: "deezer preview 429 without a wait rests five seconds",
            ask: Ask::DeezerPreview,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::Within("deezer", 4, 5)),
        },
        StatusRow {
            case: "itunes preview 429 without a wait rests a minute",
            ask: Ask::ItunesPreview,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::AMinute(itunes::SOURCE)),
        },
        StatusRow {
            case: "deezer preview 404 is absence",
            ask: Ask::DeezerPreview,
            reply: Reply::Status(404, &[], "{}"),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "itunes album tracks 404 is absence",
            ask: Ask::ItunesAlbumTracks,
            reply: Reply::Status(404, &[], "{}"),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "deezer preview 500",
            ask: Ask::DeezerPreview,
            reply: Reply::Status(500, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Server { status: 500, .. })),
            rest: None,
        },
        StatusRow {
            case: "itunes preview bad json",
            ask: Ask::ItunesPreview,
            reply: Reply::Status(200, &[], "nope"),
            expect: |r| {
                matches!(
                    r,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::Decode,
                        ..
                    })
                )
            },
            rest: None,
        },
        StatusRow {
            case: "lrclib 404 is absence",
            ask: Ask::Lyrics,
            reply: Reply::Status(
                404,
                &[],
                include_str!("../fixtures/lrclib/get_not_found.json"),
            ),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "lrclib 429 wait past thirty seconds is held to thirty",
            ask: Ask::Lyrics,
            reply: Reply::Status(429, &[("retry-after", "45")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(30)),
            rest: None,
        },
        StatusRow {
            case: "lrclib 429 without a wait rests two seconds",
            ask: Ask::Lyrics,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::Within(lrclib::SOURCE, 1, 2)),
        },
        StatusRow {
            case: "lrclib 500",
            ask: Ask::Lyrics,
            reply: Reply::Status(500, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Server { status: 500, .. })),
            rest: None,
        },
        StatusRow {
            case: "lrclib 503",
            ask: Ask::Lyrics,
            reply: Reply::Status(
                503,
                &[("retry-after", "1")],
                include_str!("../fixtures/lrclib/get_busy.json"),
            ),
            expect: |r| matches!(r, Err(ProviderError::Unavailable { .. })),
            rest: None,
        },
        StatusRow {
            case: "lrclib 400",
            ask: Ask::Lyrics,
            reply: Reply::Status(400, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::BadRequest { status: 400, .. })),
            rest: None,
        },
        StatusRow {
            case: "lrclib bad json",
            ask: Ask::Lyrics,
            reply: Reply::Status(200, &[], "nope"),
            expect: |r| {
                matches!(
                    r,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::Decode,
                        ..
                    })
                )
            },
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract 404 is absence",
            ask: Ask::WikipediaExtract,
            reply: Reply::Status(404, &[], "{}"),
            expect: |r| matches!(r, Ok(false)),
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract 500",
            ask: Ask::WikipediaExtract,
            reply: Reply::Status(500, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Server { status: 500, .. })),
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract 403",
            ask: Ask::WikipediaExtract,
            reply: Reply::Status(403, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::Forbidden { .. })),
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract bad json",
            ask: Ask::WikipediaExtract,
            reply: Reply::Status(200, &[], "nope"),
            expect: |r| {
                matches!(
                    r,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::Decode,
                        ..
                    })
                )
            },
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract unreachable",
            ask: Ask::WikipediaExtract,
            reply: Reply::Unreachable,
            expect: |r| matches!(r, Err(ProviderError::Transport { .. })),
            rest: None,
        },
        StatusRow {
            case: "wikidata entity 429 with a wait",
            ask: Ask::WikidataTitle,
            reply: Reply::Status(429, &[("retry-after", "7")], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(7)),
            rest: None,
        },
        StatusRow {
            case: "wikipedia extract 429 without a wait rests a minute",
            ask: Ask::WikipediaExtract,
            reply: Reply::Status(429, &[], "{}"),
            expect: |r| matches!(r, Err(ProviderError::RateLimited { .. })),
            rest: Some(Rest::AMinute("wikimedia-api")),
        },
    ];
    for row in rows {
        let wire = match row.reply {
            Reply::Status(status, headers, body) => {
                scripted(&row.ask.url(), &[(status, headers, body)])
            }
            Reply::Unreachable => unreachable(&row.ask.url()),
        };
        let providers = catalog_providers(&wire);
        let outcome = row.ask.send(&providers).await;
        assert!((row.expect)(&outcome), "{}: {outcome:?}", row.case);
        match row.rest {
            Some(Rest::AMinute(rate_row)) => {
                let rest = resting(&providers, rate_row);
                assert!(
                    rest >= Duration::from_secs(59),
                    "{}: rests {rest:?}",
                    row.case
                );
            }
            Some(Rest::Within(rate_row, above, at_most)) => {
                let rest = resting(&providers, rate_row);
                assert!(
                    rest > Duration::from_secs(above) && rest <= Duration::from_secs(at_most),
                    "{}: rests {rest:?}",
                    row.case
                );
            }
            None => {}
        }
    }
}

/// The "Get it" link names the album asked for, or there is none. The recorded
/// search for Nevermind holds no Nevermind by Nirvana: its rows by other
/// artists, tribute albums included, fail the artist gate, and Nirvana's own
/// "MTV Unplugged In New York" fails the title gate.
#[tokio::test]
async fn itunes_link_names_the_album_asked_for_or_none() {
    let wire = scripted(ITUNES, &[(200, &[], ITUNES_ALBUMS)]);
    let found = catalog_providers(&wire)
        .itunes
        .find_album("Nirvana", "Nevermind", "US", PRIORITY)
        .await
        .unwrap();
    assert_eq!(found, None);
    let sent = wire.sent();
    assert_eq!(sent[0].query("term").as_deref(), Some("Nirvana Nevermind"));
    assert_eq!(sent[0].query("country").as_deref(), Some("US"));

    let wire = scripted(ITUNES, &[(200, &[], ITUNES_ALBUMS)]);
    let found = catalog_providers(&wire)
        .itunes
        .find_album("Nirvana", "Nirvana", "US", PRIORITY)
        .await
        .unwrap()
        .expect("the self-titled record is in the answer");
    assert_eq!(found.collection_name, "Nirvana");
    assert!(found.url.starts_with("https://music.apple.com/us/album/"));

    assert!(itunes::token_set_ratio("Nirvana", "Piano Tribute Players") < 80.0);
}

/// The Deezer and iTunes preview legs tell absence from refusal: an album
/// Deezer does not know is absence, any other error object is a refusal, a
/// hit without a preview gives way to the next, and a dead or empty leg falls
/// through to the other.
#[tokio::test(start_paused = true)]
async fn preview_answers_tell_absence_from_refusal() {
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
    assert!(tracks.is_empty(), "deezer album not found");

    // Any other code is a refusal, never absence.
    let body = r#"{"error": {"type": "ParameterException",
        "message": "Wrong parameter", "code": 700}}"#;
    let wire = scripted(&deezer_search(), &[(200, &[], body)]);
    let error = catalog_providers(&wire)
        .previews
        .deezer_track_preview("A", "T", PRIORITY)
        .await
        .expect_err("an error object");
    assert!(
        matches!(error, ProviderError::BadRequest { status: 200, .. }),
        "deezer error object: {error:?}"
    );

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
    assert_eq!(
        found.preview_url, "https://cdn/t.mp3",
        "hit without a preview"
    );

    // Deezer empty, iTunes hit.
    let no_hits = include_str!("../fixtures/deezer/search_track_no_hits.json");
    let wire = scripted(&deezer_search(), &[(200, &[], no_hits)]);
    wire.answer(ITUNES, 200, &[], ITUNES_SONG);
    let (found, provider) = catalog_providers(&wire)
        .previews
        .get_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await;
    assert_eq!(
        found.unwrap().title,
        "Smells Like Teen Spirit",
        "deezer empty"
    );
    assert_eq!(provider, Some("itunes"), "deezer empty");

    // Deezer error still falls through to iTunes.
    let wire = unreachable(&deezer_search());
    wire.answer(ITUNES, 200, &[], ITUNES_SONG);
    let (found, provider) = catalog_providers(&wire)
        .previews
        .get_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
        .await;
    assert!(found.is_some(), "deezer unreachable");
    assert_eq!(provider, Some("itunes"), "deezer unreachable");

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
    assert_eq!(found, None, "both legs dead");
    assert_eq!(provider, None, "both legs dead");
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
