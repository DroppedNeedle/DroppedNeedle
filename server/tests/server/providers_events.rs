//! Geocoding, Skiddle, Ticketmaster, YouTube and GitHub clients over a
//! scripted wire: the decode of each live wire shape, absence told from
//! failure, Ticketmaster's page cap, and the YouTube quota file (reserved
//! before HTTP, charged for dispatched calls, kept across restarts).

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

/// Recorded on 2026-10-29: tests/fixtures/youtube/search_found.json, the
/// album search for Portishead's Dummy, one hit.
const SEARCH_FOUND: &str = include_str!("../fixtures/youtube/search_found.json");
/// The video the recorded search found.
const FOUND_VIDEO: &str = "hd9OYCqBK88";

fn answer_search_found(wire: &ScriptedWire) {
    wire.answer(&url(SEARCH), 200, &[], SEARCH_FOUND);
}

// ---------------------------------------------------------------------------
// Geocoding, Skiddle, Ticketmaster
// ---------------------------------------------------------------------------

/// None means absence, never failure: an answer saying "no such" is empty, and
/// a refusal or a broken envelope is a typed failure, never an empty list.
#[tokio::test]
async fn event_answers_tell_absence_from_failure() {
    // The geocoder knows no such place: the answer has no `results` key.
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
    assert!(cities.is_empty(), "geocoding, no such place");

    // A refused request answers HTTP 400 with `error` and `reason` and no
    // `results` key.
    let (wire, providers) = scripted();
    wire.answer(
        &url("geocoding"),
        400,
        &[],
        include_str!("../fixtures/geocoding/search_bad_count.json"),
    );
    let refused = providers
        .geocoding
        .search_cities("Liverpool", DEFAULT_CITY_COUNT, USER)
        .await;
    assert!(
        matches!(refused, Err(ProviderError::BadRequest { .. })),
        "geocoding, refused request: {refused:?}"
    );

    // An empty Skiddle results list is no match.
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
            .is_empty(),
        "skiddle, no match"
    );

    // An answer without Skiddle's error code, or a success without its
    // results, is a payload failure.
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
            "skiddle, envelope missing a part: {body}"
        );
    }

    // A key Skiddle does not know comes back as HTTP 400 with its error
    // envelope. The status decides the failure, so the envelope is never decoded.
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/artists/"),
        400,
        &[],
        include_str!("../fixtures/skiddle/artists_bad_key.json"),
    );
    let bad_key = providers.skiddle.search_artists("k", "Orbital", USER).await;
    assert!(
        matches!(bad_key, Err(ProviderError::BadRequest { status: 400, .. })),
        "skiddle, bad key: {bad_key:?}"
    );

    // Skiddle's own failure envelope arrives on HTTP 200 with `error != 0`;
    // only its code is kept.
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
        }) => assert!(
            message.contains("error 1"),
            "skiddle, error envelope on 200: {message}"
        ),
        other => panic!("skiddle, error envelope on 200: expected a payload error, got {other:?}"),
    }

    // Zero Ticketmaster results are empty answers: the attractions search
    // omits `_embedded`, and the events answer has no pages.
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
    assert!(acts.is_empty(), "ticketmaster, no acts");
    assert!(events.is_empty(), "ticketmaster, no events");
}

/// What the recorded event answers mean once read, and the lineup act's
/// MusicBrainz ids trimmed, lowercased and blanks dropped.
#[tokio::test]
async fn recorded_event_answers_read_as_answered() {
    // Skiddle events for one act: two gigs with lineups, `cancelled` as the
    // string `'0'`, and empty strings for absent ticket URLs and reschedule
    // dates.
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
    assert_eq!(events.len(), 2, "skiddle events");
    let event = &events[0];
    assert_eq!(event.id, "42612509", "skiddle events");
    assert_eq!(event.date.as_deref(), Some("2026-10-30"), "skiddle events");
    assert!(!event.is_cancelled(), "skiddle events");
    assert!(!event.is_rescheduled(), "skiddle events");
    assert_eq!(event.ticket_url.as_deref(), Some(""), "skiddle events");
    assert_eq!(
        event.venue.as_ref().and_then(|venue| venue.town.as_deref()),
        Some("Whitstable"),
        "skiddle events"
    );
    assert_eq!(event.artists.len(), 3, "skiddle events");
    assert_eq!(
        event.artists[0].artistid.as_deref(),
        Some("123487626"),
        "skiddle events"
    );
    let calls = sent_to(&wire, "skiddle/events/search/");
    assert_eq!(calls.len(), 1, "skiddle events");
    assert_eq!(
        calls[0].query("a").as_deref(),
        Some("123487626"),
        "skiddle events"
    );
    assert_eq!(
        calls[0].query("description").as_deref(),
        Some("1"),
        "skiddle events"
    );
    assert_eq!(
        calls[0].query("api_key").as_deref(),
        Some("k"),
        "skiddle events"
    );

    // The key check reads only the error code of a recorded answer.
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/events/search/"),
        200,
        &[],
        include_str!("../fixtures/skiddle/events_key_check.json"),
    );
    providers
        .skiddle
        .check_key("k", USER)
        .await
        .expect("skiddle key check");

    // Ticketmaster events for an act with 56 upcoming events: local and
    // absolute start, sale status, a venue with string coordinates, and the
    // MusicBrainz id of each lineup act.
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
    assert_eq!(events.len(), 56, "ticketmaster events");
    let event = &events[0];
    assert_eq!(event.id, "ZG9rmIYnyZdk1k", "ticketmaster events");
    assert_eq!(event.name, "Fontaines D.C.", "ticketmaster events");
    assert_eq!(
        event.url.as_deref(),
        Some(
            "https://www.ticketmaster.it/biglietti/fontaines-dc-casalecchio-di-reno-bologna-26-10-2026/event/h5qsmyc9wmts"
        ),
        "ticketmaster events"
    );
    let dates = event.dates.as_ref().unwrap();
    assert_eq!(
        dates.start.as_ref().unwrap().local_date.as_deref(),
        Some("2026-10-26"),
        "ticketmaster events"
    );
    assert_eq!(
        dates.start.as_ref().unwrap().date_time.as_deref(),
        Some("2026-10-26T20:00:00Z"),
        "ticketmaster events"
    );
    assert_eq!(
        dates.status.as_ref().unwrap().code.as_deref(),
        Some("offsale"),
        "ticketmaster events"
    );
    let inner = event.embedded.as_ref().unwrap();
    let venue = &inner.venues[0];
    assert_eq!(
        venue.name.as_deref(),
        Some("Unipol Arena"),
        "ticketmaster events"
    );
    assert_eq!(
        venue.city.as_ref().unwrap().name.as_deref(),
        Some("Casalecchio di Reno (Bologna)"),
        "ticketmaster events"
    );
    assert_eq!(
        venue.country.as_ref().unwrap().country_code.as_deref(),
        Some("IT"),
        "ticketmaster events"
    );
    let location = venue.location.as_ref().unwrap();
    assert_eq!(
        location.latitude.as_deref(),
        Some("44.485673"),
        "ticketmaster events"
    );
    assert_eq!(
        location.longitude.as_deref(),
        Some("11.248983"),
        "ticketmaster events"
    );
    assert_eq!(
        inner.attractions[0].musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"],
        "ticketmaster events"
    );
    let calls = sent_to(&wire, "ticketmaster/events.json");
    assert_eq!(
        calls[0].query("apikey").as_deref(),
        Some("k"),
        "ticketmaster events"
    );
    assert_eq!(
        calls[0].query("attractionId").as_deref(),
        Some("K8vZ9179LP7"),
        "ticketmaster events"
    );

    // A MusicBrainz link with no id is supporting evidence: it is dropped and
    // the act still reads. Padded and cased ids are trimmed and lowercased,
    // and blank ids are skipped. The body is made up, since no recorded
    // answer has a link without an id.
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
    assert_eq!(act.id, "K8vZ9179LP7", "link without an id");
    assert_eq!(act.name, "Fontaines D.C.", "link without an id");
    assert_eq!(
        act.musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"],
        "link without an id"
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

// ---------------------------------------------------------------------------
// YouTube
// ---------------------------------------------------------------------------

/// The quota governor (100 searches a day): a search reserves durably before
/// any HTTP, a refused one sends nothing, a recreated client remembers what
/// today spent, and two users on one client cannot spend the last slot twice.
#[tokio::test]
async fn youtube_quota_is_reserved_before_http_and_kept() {
    // The first search reserves durably; the second (on a fresh key) is
    // refused before any HTTP, and the status snapshot matches, dated by the
    // clock the client was given. The key goes out as the `key` query pair.
    let (wire, providers) = scripted();
    answer_search_found(&wire);
    let (_scratch, path) = quota_path("exhausted");
    let client = youtube(&providers, &path, 1);

    assert_eq!(client.quota_remaining().await, 1, "exhausted");
    assert!(client.search_available().await, "exhausted");
    assert_eq!(
        client
            .search_video("Portishead", "Dummy", USER)
            .await
            .unwrap()
            .as_deref(),
        Some(FOUND_VIDEO),
        "exhausted"
    );
    assert!(
        matches!(
            client.search_track("a", "two", USER).await,
            Err(YoutubeError::QuotaExhausted)
        ),
        "exhausted"
    );
    let calls = sent_to(&wire, SEARCH);
    assert_eq!(calls.len(), 1, "exhausted");
    assert_eq!(
        calls[0].query("key").as_deref(),
        Some("secret"),
        "exhausted"
    );
    assert_eq!(
        calls[0].query("q").as_deref(),
        Some("Portishead Dummy full album"),
        "exhausted"
    );
    assert_eq!(
        quota_file_json(&path),
        json!({"date": "2026-05-05", "count": 1}),
        "exhausted"
    );
    assert_eq!(client.quota_remaining().await, 0, "exhausted");
    assert!(!client.search_available().await, "exhausted");
    let status = client.get_quota_status().await;
    assert_eq!(status.used, 1, "exhausted");
    assert_eq!(status.limit, 1, "exhausted");
    assert_eq!(status.remaining, 0, "exhausted");
    assert_eq!(status.date, "2026-05-05", "exhausted");

    // A recreated client on the same quota path remembers what today spent,
    // and the next day by the clock starts from zero.
    let (wire, providers) = scripted();
    answer_search_found(&wire);
    let (_scratch, path) = quota_path("recreate");
    let first = youtube(&providers, &path, 1);
    first
        .search_video("a", "one", USER)
        .await
        .expect("recreated: the first search");
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
    assert!(
        matches!(
            second.search_video("a", "blocked", USER).await,
            Err(YoutubeError::QuotaExhausted)
        ),
        "recreated"
    );
    clock.advance(24 * 3600 * 1000);
    assert_eq!(second.quota_remaining().await, 1, "recreated, next day");

    // Two users sharing the one client of a quota path cannot spend the last
    // slot twice.
    let (wire, providers) = scripted();
    answer_search_found(&wire);
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
            .filter(|o| matches!(o, Ok(Some(id)) if id == FOUND_VIDEO))
            .count(),
        1,
        "last slot"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Err(YoutubeError::QuotaExhausted)))
            .count(),
        1,
        "last slot"
    );
    assert_eq!(sent_to(&wire, SEARCH).len(), 1, "last slot");
    assert_eq!(quota_file_json(&path)["count"], json!(1), "last slot");
}

/// A dispatched search that fails is charged once, never resent and never
/// cached: a 403 (the API's quota answer among others) is a refusal, an
/// unreadable body and an upstream 429 map distinctly, and while the API
/// rests after the 429 a person's search is refused before a unit is spent.
#[tokio::test]
async fn youtube_dispatched_failures_are_charged_once() {
    // A failed search stays charged and uncached; a later search for the same
    // pair tries the network again.
    let (wire, providers) = scripted();
    // Made up: the spent-quota 403 was never recorded.
    wire.answer(&url(SEARCH), 403, &[], "{}");
    answer_search_found(&wire);
    let (_scratch, path) = quota_path("charged");
    let client = youtube(&providers, &path, 2);

    assert!(
        matches!(
            client.search_video("a", "album", USER).await,
            Err(YoutubeError::Provider(ProviderError::Forbidden { .. }))
        ),
        "403"
    );
    assert_eq!(quota_file_json(&path)["count"], json!(1), "403");
    assert!(
        !client.is_cached("a", "album", SearchKind::Album).await,
        "403 is not cached"
    );
    assert_eq!(
        client
            .search_video("a", "album", USER)
            .await
            .unwrap()
            .as_deref(),
        Some(FOUND_VIDEO),
        "after the 403"
    );
    assert_eq!(sent_to(&wire, SEARCH).len(), 2, "after the 403");
    assert_eq!(quota_file_json(&path)["count"], json!(2), "after the 403");

    // Undecodable bodies and an upstream 429 are charged once each.
    let (wire, providers) = scripted();
    // Made up on purpose: a body that is not JSON, and a hit without its id.
    wire.answer(&url(SEARCH), 200, &[], "not json");
    json_answer(&wire, SEARCH, json!({"items": [{"id": {}}]}));
    // Made up: the 429 was never recorded.
    wire.answer(&url(SEARCH), 429, &[("retry-after", "30")], "{}");
    let (_scratch, path) = quota_path("upstream");
    let client = youtube(&providers, &path, 80);

    assert!(
        matches!(
            client.search_track("x", "broken", USER).await,
            Err(YoutubeError::Provider(ProviderError::Payload {
                kind: PayloadKind::Decode,
                ..
            }))
        ),
        "not json"
    );
    assert!(
        matches!(
            client.search_track("x", "no-id", USER).await,
            Err(YoutubeError::Provider(ProviderError::Payload {
                kind: PayloadKind::Decode,
                ..
            }))
        ),
        "hit without its id"
    );
    assert!(
        matches!(
            client.search_track("x", "limited", USER).await,
            Err(YoutubeError::Provider(ProviderError::RateLimited { .. }))
        ),
        "429"
    );
    // All three dispatched once, so all three were charged.
    assert_eq!(sent_to(&wire, SEARCH).len(), 3, "three dispatched");
    assert_eq!(
        quota_file_json(&path)["count"],
        json!(3),
        "three dispatched"
    );

    assert!(
        matches!(
            client.search_track("x", "resting", USER).await,
            Err(YoutubeError::Provider(ProviderError::RateLimited { .. }))
        ),
        "resting after the 429"
    );
    assert_eq!(sent_to(&wire, SEARCH).len(), 3, "resting sends nothing");
    assert_eq!(
        quota_file_json(&path)["count"],
        json!(3),
        "resting spends nothing"
    );
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

const GITHUB_RELEASES: &str = include_str!("../fixtures/github/releases_list.json");
const GITHUB_NOT_FOUND: &str = include_str!("../fixtures/github/releases_not_found.json");

fn releases_payload() -> serde_json::Value {
    serde_json::from_str(GITHUB_RELEASES).expect("the recorded list parses")
}

/// The release check reads the recorded list, filters drafts and falls back
/// for a missing name or body; failures degrade to `[]` and are not kept; and
/// a person's check never waits for a request.
#[tokio::test(start_paused = true)]
async fn github_release_check_reads_filters_and_degrades() {
    // The recorded list decodes in full. A missing name or body falls back to
    // the tag and `""`, a draft is filtered out, and the GitHub media type
    // header goes out on the call.
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

    assert_eq!(releases.len(), 29, "decode");
    assert_eq!(releases[0].tag_name, "v2.15.0", "decode");
    assert_eq!(
        releases[0].name, "v2.15.0",
        "decode: name falls back to the tag"
    );
    assert_eq!(releases[0].published_at, "2026-09-20T01:49:51Z", "decode");
    assert_eq!(
        releases[0].html_url, "https://github.com/DroppedNeedle/DroppedNeedle/releases/tag/v2.15.0",
        "decode"
    );
    assert!(!releases[0].prerelease, "decode");
    assert_eq!(releases[1].tag_name, "v2.14.1", "decode");
    assert_eq!(releases[1].body, "", "decode: body falls back to empty");
    assert_eq!(releases[2].tag_name, "v2.13.0", "decode: the draft is gone");
    let calls = sent_to(&wire, "releases");
    assert_eq!(
        calls[0].headers,
        [(
            "accept".to_owned(),
            "application/vnd.github+json".to_owned()
        )],
        "decode"
    );

    // Error answers degrade to `[]`, quietly, and are not remembered: the
    // next check asks again, while the row has a request to give, and keeps
    // what it gets.
    let (wire, providers) = scripted();
    wire.answer(&url("releases"), 404, &[], GITHUB_NOT_FOUND);
    json_answer(&wire, "releases", releases_payload());

    assert_eq!(
        providers.github.fetch_releases(USER).await,
        vec![],
        "not kept: the 404"
    );
    assert_eq!(
        providers
            .github
            .fetch_latest_release(USER)
            .await
            .map(|release| release.tag_name),
        Some("v2.15.0".to_owned()),
        "not kept: asked again"
    );
    assert_eq!(
        providers.github.fetch_releases(USER).await.len(),
        30,
        "not kept: the answer is kept"
    );
    assert_eq!(sent_to(&wire, "releases").len(), 2, "not kept");

    // A person's check never waits for a request. Once failing answers have
    // spent the row's burst (50 an hour, so the next request is 72 s away),
    // the check answers `[]` at once and sends nothing.
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&url("releases"), 404, &[], "{}");
    let providers = Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        droppedneedle_providers::ProviderEndpoints::closed(),
    );
    let burst = droppedneedle_providers::rates::row("github").unwrap().burst as usize;
    for _ in 0..burst {
        assert_eq!(providers.github.fetch_releases(USER).await, vec![], "burst");
    }
    assert_eq!(sent_to(&wire, "releases").len(), burst, "burst");

    let started = tokio::time::Instant::now();
    assert_eq!(
        providers.github.fetch_releases(USER).await,
        vec![],
        "past the burst"
    );
    assert!(
        started.elapsed() <= droppedneedle_providers::executor::USER_COOLDOWN_WAIT,
        "past the burst: waited {:?}",
        started.elapsed()
    );
    assert_eq!(sent_to(&wire, "releases").len(), burst, "nothing sent");
}
