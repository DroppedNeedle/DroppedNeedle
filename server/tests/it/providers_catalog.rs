//! Discogs, iTunes, preview (Deezer and iTunes), LRCLIB, Internet Archive
//! and Wikidata clients over a scripted transport: the decode of each real
//! wire shape, identity gates, rate limits and degradation.

use droppedneedle::providers::{HttpFault, HttpPort, HttpReply};
use droppedneedle::providers::{archive, discogs, itunes, lrclib, preview, wikidata};

// ---------------------------------------------------------------------------
// Scripted fakes
// ---------------------------------------------------------------------------

/// Generate a recording fake transport for one provider module. All
/// fakes implement the one shared [`HttpPort`].
macro_rules! scripted_client {
    ($client:ident) => {
        struct $client {
            replies: std::sync::Mutex<std::collections::VecDeque<Result<HttpReply, HttpFault>>>,
            calls: std::sync::Mutex<Vec<(String, Vec<(String, String)>)>>,
        }

        impl $client {
            fn scripted(replies: Vec<Result<HttpReply, HttpFault>>) -> Self {
                Self {
                    replies: std::sync::Mutex::new(replies.into_iter().collect()),
                    calls: std::sync::Mutex::new(Vec::new()),
                }
            }

            #[allow(dead_code)] // Only some providers assert on their requests.
            fn calls(&self) -> Vec<(String, Vec<(String, String)>)> {
                self.calls.lock().unwrap().clone()
            }
        }

        impl HttpPort for $client {
            async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
                self.calls.lock().unwrap().push((
                    url.to_owned(),
                    query
                        .iter()
                        .map(|(key, value)| (key.to_string(), value.to_string()))
                        .collect(),
                ));
                self.replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("scripted client ran out of replies")
            }
        }
    };
}

/// Generate reply constructors for one provider module.
macro_rules! reply_fns {
    ($plain:ident, $retry:ident, $fault:ident) => {
        fn $plain(status: u16, body: &str) -> Result<HttpReply, HttpFault> {
            Ok(HttpReply {
                status,
                body: body.as_bytes().to_vec(),
                retry_after: None,
            })
        }

        fn $retry(status: u16, body: &str, retry_after: &str) -> Result<HttpReply, HttpFault> {
            Ok(HttpReply {
                status,
                body: body.as_bytes().to_vec(),
                retry_after: Some(retry_after.to_owned()),
            })
        }

        fn $fault() -> Result<HttpReply, HttpFault> {
            Err(HttpFault)
        }
    };
}

scripted_client!(ScriptDiscogs);
scripted_client!(ScriptITunes);
scripted_client!(ScriptPreview);
scripted_client!(ScriptLrclib);
scripted_client!(ScriptArchive);
scripted_client!(ScriptWiki);

reply_fns!(dreply, dreply_retry, dfault);
reply_fns!(ireply, ireply_retry, _ifault);
reply_fns!(preply, preply_retry, pfault);
reply_fns!(lreply, lreply_retry, _lfault);
reply_fns!(areply, areply_retry, _afault);
reply_fns!(wreply, wreply_retry, wfault);

// ---------------------------------------------------------------------------
// Discogs
// ---------------------------------------------------------------------------

const DISCOGS_RELEASE: &str = r#"{
    "id": 249504, "master_id": 96559, "title": "Never Gonna Give You Up",
    "artists_sort": "Rick Astley",
    "artists": [{"id": 22, "name": "Rick Astley", "anv": "", "join": ""}],
    "year": 1987, "country": "UK", "released": "1987-07-27",
    "labels": [{"id": 7, "name": "RCA", "catno": "PB 41447"}],
    "formats": [{"name": "Vinyl", "qty": "1", "descriptions": ["7\""], "text": ""}],
    "identifiers": [{"type": "Barcode", "value": "5 012394-144777", "description": ""}],
    "tracklist": [
        {"position": "A", "type_": "track", "title": "Never Gonna Give You Up",
         "duration": "3:32", "artists": [], "sub_tracks": []},
        {"position": "B", "type_": "track", "title": "You Move Me",
         "duration": "3:40", "artists": [], "sub_tracks": []}
    ],
    "images": [{"uri": "https://example.invalid/restricted.jpg"}],
    "community": {"want": 1}, "lowest_price": 4.5, "future_field": {"n": 1}
}"#;

#[tokio::test]
async fn discogs_contract_tolerates_unknown_fields() {
    let http = ScriptDiscogs::scripted(vec![dreply(200, DISCOGS_RELEASE)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    let release = client.get_release("249504", 1.0).await.unwrap().unwrap();
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
        let http = ScriptDiscogs::scripted(vec![dreply(200, body)]);
        let client = discogs::DiscogsClient::with_base(&http, "http://fake");
        assert_eq!(
            client.get_release("1", 1.0).await,
            Err(discogs::FetchError::Unusable),
            "body: {body}"
        );
    }
}

#[tokio::test]
async fn discogs_degradation_429_keeps_retry_after() {
    for (reply, expected) in [
        (dreply_retry(429, "{}", "7"), 7.0),
        (dreply(429, "{}"), 60.0),
        (dreply_retry(429, "{}", "junk"), 60.0),
        (dreply_retry(429, "{}", "0.2"), 1.0),
    ] {
        let http = ScriptDiscogs::scripted(vec![reply]);
        let client = discogs::DiscogsClient::with_base(&http, "http://fake");
        assert_eq!(
            client.get_release("1", 1.0).await,
            Err(discogs::FetchError::RateLimited {
                retry_after_secs: expected
            })
        );
    }
}

#[tokio::test]
async fn discogs_degradation_500_transport_bad_json_unusable() {
    let http = ScriptDiscogs::scripted(vec![dreply(500, "{}")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_release("1", 1.0).await,
        Err(discogs::FetchError::Transport)
    );

    let http = ScriptDiscogs::scripted(vec![dfault()]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_release("1", 1.0).await,
        Err(discogs::FetchError::Transport)
    );

    let http = ScriptDiscogs::scripted(vec![dreply(200, "not-json")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_release("1", 1.0).await,
        Err(discogs::FetchError::Unusable)
    );

    let http = ScriptDiscogs::scripted(vec![dreply(418, "{}")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_release("1", 1.0).await,
        Err(discogs::FetchError::Unusable)
    );
}

// ---------------------------------------------------------------------------
// iTunes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn itunes_contract_tolerates_unknown_fields() {
    let body = r#"{"resultCount": 1, "future": true, "results": [
        {"wrapperType": "collection", "collectionType": "Album",
         "artistName": "Nirvana", "collectionName": "Nevermind",
         "collectionViewUrl": "https://music.apple.com/x?uo=4",
         "collectionPrice": 9.99, "currency": "USD", "extra": {"a": 1}}
    ]}"#;
    let http = ScriptITunes::scripted(vec![ireply(200, body)]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    let found = client
        .find_album("Nirvana", "Nevermind", "US")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.url, "https://music.apple.com/x?uo=4");
    assert_eq!(found.collection_name, "Nevermind");
    assert_eq!(found.artist_name, "Nirvana");
}

#[tokio::test]
async fn itunes_quirk_tribute_album_rejected_real_album_kept() {
    // Live ranking puts tributes above the real record (ITUNES_API_NOTES.md);
    // the fuzzy artist gate must skip them.
    let body = r#"{"resultCount": 2, "results": [
        {"wrapperType": "collection", "artistName": "Piano Tribute Players",
         "collectionName": "Piano Tribute to Nirvana",
         "collectionViewUrl": "https://music.apple.com/tribute"},
        {"wrapperType": "collection", "artistName": "Nirvana",
         "collectionName": "Nevermind",
         "collectionViewUrl": "https://music.apple.com/real"}
    ]}"#;
    let http = ScriptITunes::scripted(vec![ireply(200, body)]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    let found = client
        .find_album("Nirvana", "Nevermind", "US")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.url, "https://music.apple.com/real");

    assert!(itunes::token_set_ratio("Nirvana", "Piano Tribute Players") < 80.0);
    assert!(itunes::token_set_ratio("Nirvana", "Nirvana") >= 80.0);
}

#[tokio::test]
async fn itunes_degradation_429_500_bad_json() {
    let http = ScriptITunes::scripted(vec![ireply_retry(429, "{}", "9")]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(
        client.find_album("A", "B", "US").await,
        Err(itunes::FetchError::RateLimited {
            retry_after_secs: 9.0
        })
    );

    let http = ScriptITunes::scripted(vec![ireply(429, "{}")]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(
        client.find_album("A", "B", "US").await,
        Err(itunes::FetchError::RateLimited {
            retry_after_secs: 60.0
        })
    );

    let http = ScriptITunes::scripted(vec![ireply(500, "{}")]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(
        client.find_album("A", "B", "US").await,
        Err(itunes::FetchError::Unusable)
    );

    let http = ScriptITunes::scripted(vec![ireply(200, "<html>")]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(
        client.find_album("A", "B", "US").await,
        Err(itunes::FetchError::Unusable)
    );
}

// ---------------------------------------------------------------------------
// Preview (Deezer + iTunes)
// ---------------------------------------------------------------------------

fn preview_client(http: &ScriptPreview) -> preview::PreviewClient<'_, ScriptPreview> {
    preview::PreviewClient::with_bases(http, "http://fake/deezer", "http://fake/itunes")
}

#[tokio::test]
async fn preview_contract_tolerant_decode() {
    let body = r#"{"data": [
        {"id": 1, "title": "T", "title_short": "T", "duration": 200,
         "track_position": 2, "preview": "https://cdn/x.mp3?hdnea=exp=1",
         "artist": {"id": 9, "name": "A"}, "unknown": [1, 2]}
    ], "total": 1, "next": null}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .deezer_track_preview("A", "T")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.position, Some(2));

    let body = r#"{"resultCount": 1, "results": [
        {"artistName": "A", "trackName": "T", "collectionName": "C",
         "previewUrl": "https://audio/x.m4a", "trackTimeMillis": 30000,
         "trackNumber": 4, "primaryGenreName": "Rock", "futureField": {"n": 1}}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .itunes_track_preview("A", "T")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.title, "T");
    assert_eq!(found.duration_s, Some(30));
}

#[tokio::test]
async fn preview_quirk_track_fallback_chain() {
    // Deezer empty, iTunes hit.
    let itunes = r#"{"resultCount": 1, "results": [
        {"artistName": "A", "trackName": "T", "collectionName": "C",
         "previewUrl": "https://audio/t.m4a"}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, r#"{"data": []}"#), preply(200, itunes)]);
    let (found, provider) = preview_client(&http).get_track_preview("A", "T").await;
    assert_eq!(found.unwrap().preview_url, "https://audio/t.m4a");
    assert_eq!(provider, Some("itunes"));

    // Deezer error still falls through to iTunes.
    let http = ScriptPreview::scripted(vec![pfault(), preply(200, itunes)]);
    let (found, provider) = preview_client(&http).get_track_preview("A", "T").await;
    assert!(found.is_some());
    assert_eq!(provider, Some("itunes"));

    // Both legs dead is quiet absence.
    let http = ScriptPreview::scripted(vec![pfault(), pfault()]);
    let (found, provider) = preview_client(&http).get_track_preview("A", "T").await;
    assert_eq!(found, None);
    assert_eq!(provider, None);
}

#[tokio::test]
async fn preview_degradation_leg_errors() {
    let http = ScriptPreview::scripted(vec![preply_retry(429, "{}", "1")]);
    assert_eq!(
        preview_client(&http).deezer_track_preview("A", "T").await,
        Err(preview::FetchError::RateLimited {
            retry_after_secs: 1.0
        })
    );

    let http = ScriptPreview::scripted(vec![preply(429, "{}")]);
    assert_eq!(
        preview_client(&http).deezer_track_preview("A", "T").await,
        Err(preview::FetchError::RateLimited {
            retry_after_secs: 5.0
        })
    );

    let http = ScriptPreview::scripted(vec![preply_retry(429, "{}", "1")]);
    assert_eq!(
        preview_client(&http).itunes_track_preview("A", "T").await,
        Err(preview::FetchError::RateLimited {
            retry_after_secs: 1.0
        })
    );

    let http = ScriptPreview::scripted(vec![preply(429, "{}")]);
    assert_eq!(
        preview_client(&http).itunes_track_preview("A", "T").await,
        Err(preview::FetchError::RateLimited {
            retry_after_secs: 60.0
        })
    );

    let http = ScriptPreview::scripted(vec![preply(404, "{}")]);
    assert_eq!(
        preview_client(&http)
            .deezer_track_preview("A", "T")
            .await
            .unwrap(),
        None
    );

    let http = ScriptPreview::scripted(vec![preply(404, "{}")]);
    assert!(
        preview_client(&http)
            .itunes_album_tracks("A", "C", 4)
            .await
            .unwrap()
            .is_empty()
    );

    let http = ScriptPreview::scripted(vec![preply(500, "{}")]);
    assert_eq!(
        preview_client(&http).deezer_track_preview("A", "T").await,
        Err(preview::FetchError::Transport)
    );

    let http = ScriptPreview::scripted(vec![preply(200, "nope")]);
    assert_eq!(
        preview_client(&http).itunes_track_preview("A", "T").await,
        Err(preview::FetchError::Unusable)
    );
}

// ---------------------------------------------------------------------------
// LRCLIB
// ---------------------------------------------------------------------------

const LRCLIB_HIT: &str = r#"{
    "id": 42, "name": "x", "trackName": "I Don't Want to Die Tonight",
    "artistName": "Anthony Green", "albumName": "Boom. Done.",
    "duration": 197.0, "instrumental": false,
    "plainLyrics": "line one\nline two", "syncedLyrics": null,
    "lyricsfile": null, "future": 1
}"#;

#[tokio::test]
async fn lrclib_contract_tolerates_unknown_fields() {
    let http = ScriptLrclib::scripted(vec![lreply(200, LRCLIB_HIT)]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    let found = client
        .get_exact_lyrics(
            "I Don't Want to Die Tonight",
            "Anthony Green",
            "Boom. Done.",
            197,
        )
        .await
        .unwrap();
    assert!(found.found);
    let candidate = found.candidate.unwrap();
    assert_eq!(candidate.provider_id, 42);
    assert_eq!(
        candidate.plain_lyrics.as_deref(),
        Some("line one\nline two")
    );
    assert_eq!(candidate.synced_lyrics, None);
    assert!(!candidate.instrumental);
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
        let http = ScriptLrclib::scripted(vec![lreply(200, &body)]);
        let client = lrclib::LrclibClient::with_base(&http, "http://fake");
        assert_eq!(
            client.get_exact_lyrics("T", "A", "C", 120).await,
            Err(lrclib::FetchError::Unusable),
            "body: {body}"
        );
    }
}

#[tokio::test]
async fn lrclib_quirk_oversized_rejected() {
    let huge = vec![b'x'; lrclib::MAX_LYRICS_BYTES + 1];
    let http = ScriptLrclib::scripted(vec![Ok(HttpReply {
        status: 200,
        body: huge,
        retry_after: None,
    })]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 60).await,
        Err(lrclib::FetchError::Unusable)
    );

    let big = "x".repeat(lrclib::MAX_LYRICS_CHARACTERS + 1);
    let body = format!(
        r#"{{"id": 1, "trackName": "T", "artistName": "A", "albumName": "C",
            "duration": 60.0, "instrumental": false, "plainLyrics": "{big}"}}"#
    );
    let http = ScriptLrclib::scripted(vec![lreply(200, &body)]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 60).await,
        Err(lrclib::FetchError::Unusable)
    );
}

#[tokio::test]
async fn lrclib_degradation_404_429_500_bad_json() {
    let http = ScriptLrclib::scripted(vec![lreply(404, "{}")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert!(
        !client
            .get_exact_lyrics("T", "A", "C", 1)
            .await
            .unwrap()
            .found
    );

    let http = ScriptLrclib::scripted(vec![lreply_retry(429, "{}", "45")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 1).await,
        Err(lrclib::FetchError::RateLimited {
            retry_after_secs: 30.0
        })
    );

    let http = ScriptLrclib::scripted(vec![lreply(500, "{}")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 1).await,
        Err(lrclib::FetchError::Transport)
    );

    let http = ScriptLrclib::scripted(vec![lreply(200, "nope")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 1).await,
        Err(lrclib::FetchError::Unusable)
    );
}

// ---------------------------------------------------------------------------
// Internet Archive
// ---------------------------------------------------------------------------

const ARCHIVE_CC: &str = "http://creativecommons.org/licenses/by-nc-sa/3.0/";
const ARCHIVE_PD: &str = "https://creativecommons.org/publicdomain/zero/1.0/";

fn archive_client(http: &ScriptArchive) -> archive::ArchiveClient<'_, ScriptArchive> {
    archive::ArchiveClient::with_endpoints(
        http,
        "http://fake/advancedsearch.php",
        "http://fake/metadata/{identifier}",
    )
}

#[test]
fn archive_contract_licence_table() {
    // The v2 brief table, ported case for case.
    for (value, expected) in [
        (Some(ARCHIVE_CC), true),
        (Some(ARCHIVE_PD), true),
        (Some("https://creativecommons.org/licenses/by/4.0/"), true),
        (Some(""), false),
        (None, false),
        (Some("http://example.com/all-rights-reserved"), false),
        (
            Some("https://evil.example/creativecommons.org/licenses/by/4.0/"),
            false,
        ),
    ] {
        assert_eq!(
            archive::is_open_licence(value),
            expected,
            "value: {value:?}"
        );
    }
}

#[tokio::test]
async fn archive_contract_search_drops_unlicensed_items() {
    let body = format!(
        r#"{{"response": {{"docs": [
            {{"identifier": "good", "title": "Good", "licenseurl": "{ARCHIVE_CC}", "futureField": 1}},
            {{"identifier": "unlicensed", "title": "Bad"}},
            {{"identifier": "closed", "title": "Bad", "licenseurl": "http://x/arr"}},
            {{"title": "No Id", "licenseurl": "{ARCHIVE_CC}"}}
        ]}}}}"#
    );
    let http = ScriptArchive::scripted(vec![areply(200, &body)]);
    let items = archive_client(&http)
        .search_audio("A", "B", 12)
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].identifier, "good");
    assert_eq!(items[0].licence_url, ARCHIVE_CC);
}

#[tokio::test]
async fn archive_quirk_dark_and_closed_items_yield_nothing() {
    // A removed item answers `{}` rather than 404.
    let http = ScriptArchive::scripted(vec![areply(200, "{}")]);
    assert_eq!(
        archive_client(&http).get_item_files("gone").await.unwrap(),
        (String::new(), Vec::new())
    );

    let body = r#"{"metadata": {"licenseurl": ""},
        "files": [{"name": "a.mp3", "format": "MP3"}]}"#;
    let http = ScriptArchive::scripted(vec![areply(200, body)]);
    assert_eq!(
        archive_client(&http).get_item_files("x").await.unwrap(),
        (String::new(), Vec::new())
    );
}

#[tokio::test]
async fn archive_degradation_429_500_bad_json() {
    let http = ScriptArchive::scripted(vec![areply_retry(429, "{}", "1")]);
    assert_eq!(
        archive_client(&http).search_audio("A", "B", 12).await,
        Err(archive::FetchError::RateLimited {
            retry_after_secs: 1.0
        })
    );

    let http = ScriptArchive::scripted(vec![areply(429, "{}")]);
    assert_eq!(
        archive_client(&http).search_audio("A", "B", 12).await,
        Err(archive::FetchError::RateLimited {
            retry_after_secs: 60.0
        })
    );

    let http = ScriptArchive::scripted(vec![areply(500, "{}")]);
    assert_eq!(
        archive_client(&http).search_audio("A", "B", 12).await,
        Err(archive::FetchError::Unusable)
    );

    let http = ScriptArchive::scripted(vec![areply(200, "<html>")]);
    assert_eq!(
        archive_client(&http).search_audio("A", "B", 12).await,
        Err(archive::FetchError::Unusable)
    );
}

// ---------------------------------------------------------------------------
// Wikidata
// ---------------------------------------------------------------------------

fn wiki_client(http: &ScriptWiki) -> wikidata::WikidataClient<'_, ScriptWiki> {
    wikidata::WikidataClient::with_bases(
        http,
        "http://fake/data",
        "http://fake/{lang}.wiki",
        "http://fake/commons",
    )
}

#[tokio::test]
async fn wikidata_contract_tolerant_decode() {
    let entity = r#"{"entities": {"Q1": {
        "futureField": {"n": 1},
        "sitelinks": {"enwiki": {"title": "One", "badges": [], "futureField": {"n": 1}}},
        "labels": {"en": {"value": "One"}}
    }}, "success": 1}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, entity)]);
    let title = wiki_client(&http)
        .wikipedia_title_from_wikidata("Q1", "en")
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("One"));
}

#[tokio::test]
async fn wikidata_quirk_bio_hop() {
    // A Wikidata URL hops entity -> sitelink -> extract.
    let entity = r#"{"entities": {"Q42": {
        "sitelinks": {"enwiki": {"title": "Douglas Adams"}}}}}"#;
    let page = r#"{"query": {"pages": {"1": {
        "pageid": 1, "extract": "Douglas Adams was an author."}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, entity), wreply(200, page)]);
    let found = wiki_client(&http)
        .get_bio_extract("https://www.wikidata.org/wiki/Q42", "en")
        .await
        .unwrap();
    assert_eq!(found.as_deref(), Some("Douglas Adams was an author."));
    let calls = http.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0,
        "http://fake/data/wiki/Special:EntityData/Q42.json"
    );
    assert_eq!(calls[1].0, "http://fake/en.wiki/w/api.php");
    assert!(
        calls[1]
            .1
            .contains(&("titles".to_owned(), "Douglas Adams".to_owned()))
    );

    // A Wikipedia URL skips the entity hop.
    let http = ScriptWiki::scripted(vec![wreply(200, page)]);
    let found = wiki_client(&http)
        .get_bio_extract("https://en.wikipedia.org/wiki/Douglas_Adams", "en")
        .await
        .unwrap();
    assert!(found.is_some());
    assert_eq!(http.calls().len(), 1);
}

#[tokio::test]
async fn wikidata_degradation_404_absent_500_transport_bad_json() {
    let http = ScriptWiki::scripted(vec![wreply(404, "{}")]);
    assert_eq!(
        wiki_client(&http)
            .wikipedia_extract("Ghost", "en")
            .await
            .unwrap(),
        None
    );

    let http = ScriptWiki::scripted(vec![wreply(500, "{}")]);
    assert_eq!(
        wiki_client(&http).wikipedia_extract("X", "en").await,
        Err(wikidata::FetchError::Transport)
    );

    let http = ScriptWiki::scripted(vec![wreply(200, "nope")]);
    assert_eq!(
        wiki_client(&http).wikipedia_extract("X", "en").await,
        Err(wikidata::FetchError::Unusable)
    );

    let http = ScriptWiki::scripted(vec![wfault()]);
    assert_eq!(
        wiki_client(&http).wikipedia_extract("X", "en").await,
        Err(wikidata::FetchError::Transport)
    );
}

#[tokio::test]
async fn wikidata_degradation_429_honors_retry_after() {
    let http = ScriptWiki::scripted(vec![wreply_retry(429, "{}", "7")]);
    assert_eq!(
        wiki_client(&http)
            .wikipedia_title_from_wikidata("Q1", "en")
            .await,
        Err(wikidata::FetchError::RateLimited {
            retry_after_secs: 7.0
        })
    );

    let http = ScriptWiki::scripted(vec![wreply(429, "{}")]);
    assert_eq!(
        wiki_client(&http).wikipedia_extract("X", "en").await,
        Err(wikidata::FetchError::RateLimited {
            retry_after_secs: 60.0
        })
    );
}
