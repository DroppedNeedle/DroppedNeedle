//! Catalog-provider briefs (stage 5, catalog slice).
//!
//! One brief per contract, quirk, and degradation path for the six catalog
//! providers. Every test drives its provider through a scripted fake
//! transport: no live network, no shared fixtures, each reply canned inline.

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
async fn discogs_contract_empty_object_decodes_but_never_normalizes() {
    // The wire struct tolerates `{}` (every field has a default), yet the
    // result can never pass normalization: no default() empties escape.
    let wire: discogs::WireRelease = serde_json::from_str("{}").unwrap();
    assert!(discogs::normalize_release(&wire, 1.0).is_err());
    let http = ScriptDiscogs::scripted(vec![dreply(200, "{}")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_release("1", 1.0).await,
        Err(discogs::FetchError::Unusable)
    );
}

#[tokio::test]
async fn discogs_quirk_side_letters_group_into_media() {
    assert_eq!(discogs::parse_position("A1", 9), (1, Some(1)));
    assert_eq!(discogs::parse_position("b2", 9), (1, Some(2)));
    assert_eq!(discogs::parse_position("C1", 9), (2, Some(1)));
    assert_eq!(discogs::parse_position("D4", 9), (2, Some(4)));
    assert_eq!(discogs::parse_position("E1", 9), (3, Some(1)));
    // A bare side letter takes the fallback number.
    assert_eq!(discogs::parse_position("B", 7), (1, Some(7)));
    assert_eq!(discogs::parse_position("4", 7), (1, Some(4)));
    assert_eq!(discogs::parse_position("nonsense", 7), (1, None));
}

#[tokio::test]
async fn discogs_quirk_disc_track_positions() {
    assert_eq!(discogs::parse_position("2-3", 9), (2, Some(3)));
    assert_eq!(discogs::parse_position("2.3", 9), (2, Some(3)));
    assert_eq!(discogs::parse_position("0-1", 9), (1, Some(1)));
}

#[tokio::test]
async fn discogs_quirk_headings_and_subtracks() {
    let body = r#"{
        "id": 1, "title": "T",
        "tracklist": [
            {"position": "", "type_": "heading", "title": "Side A",
             "duration": "", "artists": [], "sub_tracks": []},
            {"position": "A1", "type_": "track", "title": "One",
             "duration": "1:00", "artists": [],
             "sub_tracks": [
                 {"position": "A1.1", "type_": "index", "title": "Intro",
                  "duration": "0:10", "artists": [], "sub_tracks": []}
             ]}
        ]
    }"#;
    let http = ScriptDiscogs::scripted(vec![dreply(200, body)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    let release = client.get_release("1", 1.0).await.unwrap().unwrap();
    let tracks = &release.media[0].tracks;
    assert_eq!(tracks.len(), 3);
    assert!(tracks[0].heading);
    assert_eq!(tracks[0].number, None);
    assert!(!tracks[1].heading);
    assert_eq!(tracks[1].number, Some(1));
    assert_eq!(tracks[2].title, "Intro");
}

#[test]
fn discogs_quirk_duration_shapes() {
    assert_eq!(discogs::duration_seconds("3:32"), Some(212.0));
    assert_eq!(discogs::duration_seconds("1:02:03"), Some(3723.0));
    assert_eq!(discogs::duration_seconds("3:99"), None);
    assert_eq!(discogs::duration_seconds("1:99:00"), None);
    assert_eq!(discogs::duration_seconds("nope"), None);
    assert_eq!(discogs::duration_seconds(""), None);
    assert_eq!(discogs::duration_seconds("1:2:3:4"), None);
}

#[tokio::test]
async fn discogs_quirk_format_qty_and_barcode() {
    let body = r#"{
        "id": 1, "title": "T",
        "formats": [{"name": "Vinyl", "qty": "two", "descriptions": [], "text": ""}],
        "identifiers": [{"type": "barcode", "value": " 5012-394 ", "description": ""}],
        "tracklist": []
    }"#;
    let http = ScriptDiscogs::scripted(vec![dreply(200, body)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    let release = client.get_release("1", 1.0).await.unwrap().unwrap();
    assert_eq!(release.formats[0].quantity, None);
    assert_eq!(release.barcode.as_deref(), Some("5012394"));
}

#[tokio::test]
async fn discogs_quirk_search_splits_title_and_summarizes_formats() {
    let body = r#"{"results": [
        {"id": 1, "title": "Rick Astley - Never Gonna Give You Up",
         "year": 1987, "country": "UK", "label": ["RCA"], "catno": "PB 1",
         "format": ["Vinyl", "Single"], "formats": []},
        {"id": 2, "title": "Untitled Jam", "format": [],
         "formats": [{"name": "CD", "qty": "1", "descriptions": []}]},
        {"id": 0, "title": "Ghost"}, {"id": 3, "title": ""}
    ]}"#;
    let http = ScriptDiscogs::scripted(vec![dreply(200, body)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    let hits = client
        .search_releases("Rick Astley", 10, 2.0)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Never Gonna Give You Up");
    assert_eq!(hits[0].artist_name, "Rick Astley");
    assert_eq!(hits[0].format_summary.as_deref(), Some("Vinyl, Single"));
    assert_eq!(hits[0].canonical_url, "https://www.discogs.com/release/1");
    assert_eq!(hits[1].title, "Untitled Jam");
    assert_eq!(hits[1].artist_name, "");
    assert_eq!(hits[1].format_summary.as_deref(), Some("CD"));
}

#[tokio::test]
async fn discogs_quirk_search_query_collapsed_and_clamped() {
    let http = ScriptDiscogs::scripted(vec![dreply(200, r#"{"results": []}"#)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    client
        .search_releases("  Rick   Astley  ", 99, 1.0)
        .await
        .unwrap();
    let calls = http.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "http://fake/database/search");
    assert!(
        calls[0]
            .1
            .contains(&("type".to_owned(), "release".to_owned()))
    );
    assert!(
        calls[0]
            .1
            .contains(&("q".to_owned(), "Rick Astley".to_owned()))
    );
    assert!(
        calls[0]
            .1
            .contains(&("per_page".to_owned(), "10".to_owned()))
    );

    let http = ScriptDiscogs::scripted(vec![dreply(200, r#"{"results": []}"#)]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    client.search_releases("x", 0, 1.0).await.unwrap();
    assert!(
        http.calls()[0]
            .1
            .contains(&("per_page".to_owned(), "1".to_owned()))
    );

    assert_eq!(discogs::normalize_query("  a   b  "), "a b");
    assert_eq!(
        discogs::normalize_query(&"w ".repeat(300)).chars().count(),
        200
    );
}

#[tokio::test]
async fn discogs_degradation_404_is_absence() {
    let http = ScriptDiscogs::scripted(vec![dreply(404, "{}")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert_eq!(client.get_release("1", 1.0).await.unwrap(), None);

    let http = ScriptDiscogs::scripted(vec![dreply(404, "{}")]);
    let client = discogs::DiscogsClient::with_base(&http, "http://fake");
    assert!(
        client
            .search_releases("x", 5, 1.0)
            .await
            .unwrap()
            .is_empty()
    );
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
async fn itunes_contract_skips_rows_without_album_identity() {
    let body = r#"{"resultCount": 4, "results": [
        {"wrapperType": "track", "artistName": "Nirvana",
         "collectionViewUrl": "https://music.apple.com/song"},
        {"wrapperType": "collection", "artistName": "Nirvana",
         "collectionViewUrl": ""},
        {"wrapperType": "collection", "artistName": "",
         "collectionViewUrl": "https://music.apple.com/anon"},
        {"wrapperType": "collection", "artistName": "Nirvana",
         "collectionName": "", "collectionViewUrl": "https://music.apple.com/ok"}
    ]}"#;
    let http = ScriptITunes::scripted(vec![ireply(200, body)]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    let found = client
        .find_album("Nirvana", "Nevermind", "US")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.url, "https://music.apple.com/ok");
}

#[tokio::test]
async fn itunes_contract_blank_term_makes_no_request() {
    let http = ScriptITunes::scripted(vec![]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(client.find_album("", "  ", "US").await.unwrap(), None);
    assert!(http.calls().is_empty());

    let http = ScriptITunes::scripted(vec![ireply(200, "{}")]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    assert_eq!(
        client
            .find_album("Nirvana", "Nevermind", "US")
            .await
            .unwrap(),
        None
    );
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
async fn itunes_quirk_collection_name_falls_back_to_request() {
    let body = r#"{"resultCount": 1, "results": [
        {"wrapperType": "collection", "artistName": "Nirvana",
         "collectionName": "", "collectionViewUrl": "https://music.apple.com/x"}
    ]}"#;
    let http = ScriptITunes::scripted(vec![ireply(200, body)]);
    let client = itunes::ITunesClient::with_search_url(&http, "http://fake/search");
    let found = client
        .find_album("Nirvana", "Nevermind", "GB")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.collection_name, "Nevermind");

    let calls = http.calls();
    assert_eq!(calls[0].0, "http://fake/search");
    assert!(
        calls[0]
            .1
            .contains(&("entity".to_owned(), "album".to_owned()))
    );
    assert!(
        calls[0]
            .1
            .contains(&("country".to_owned(), "GB".to_owned()))
    );
    assert!(calls[0].1.contains(&("limit".to_owned(), "10".to_owned())));
    assert!(
        calls[0]
            .1
            .contains(&("term".to_owned(), "Nirvana Nevermind".to_owned()))
    );
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
async fn preview_contract_empty_pages_are_absence() {
    let http = ScriptPreview::scripted(vec![preply(200, r#"{"data": []}"#)]);
    assert_eq!(
        preview_client(&http)
            .deezer_track_preview("A", "T")
            .await
            .unwrap(),
        None
    );

    let http = ScriptPreview::scripted(vec![preply(200, "{}")]);
    assert!(
        preview_client(&http)
            .deezer_album_tracks("A", "C", 4)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn preview_quirk_deezer_scoped_query_shape() {
    let http = ScriptPreview::scripted(vec![preply(200, r#"{"data": []}"#)]);
    preview_client(&http)
        .deezer_track_preview("Brad Sucks", "Guess")
        .await
        .unwrap();
    let calls = http.calls();
    assert_eq!(calls[0].0, "http://fake/deezer/search");
    assert!(calls[0].1.contains(&(
        "q".to_owned(),
        r#"artist:"Brad Sucks" track:"Guess""#.to_owned()
    )));
    assert!(calls[0].1.contains(&("limit".to_owned(), "3".to_owned())));
}

#[tokio::test]
async fn preview_quirk_deezer_first_preview_wins() {
    let body = r#"{"data": [
        {"title": "Nope", "title_short": "", "preview": "",
         "artist": {"name": "A"}},
        {"title": "Yep Full", "title_short": "Yep",
         "preview": "https://cdn/y.mp3", "track_position": 5,
         "artist": {"name": "A"}}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .deezer_track_preview("A", "Yep")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.title, "Yep");
    assert_eq!(found.position, Some(5));
    assert_eq!(found.duration_s, Some(30));
}

#[tokio::test]
async fn preview_quirk_deezer_album_artist_match() {
    let albums = r#"{"data": [
        {"id": 1, "title": "C", "artist": {"name": "Someone Else"}},
        {"id": 2, "title": "C", "artist": {"name": "Brad Sucks"}}
    ]}"#;
    let tracks = r#"{"data": [
        {"title": "One", "title_short": "", "preview": "https://cdn/1.mp3",
         "track_position": 1, "artist": {"name": "Brad Sucks"}},
        {"title": "Two", "title_short": "", "preview": "",
         "track_position": 2, "artist": {"name": "Brad Sucks"}},
        {"title": "Three", "title_short": "", "preview": "https://cdn/3.mp3",
         "artist": {"name": "Brad Sucks"}}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, albums), preply(200, tracks)]);
    let found = preview_client(&http)
        .deezer_album_tracks("Brad Sucks", "C", 4)
        .await
        .unwrap();
    let calls = http.calls();
    assert_eq!(calls[1].0, "http://fake/deezer/album/2/tracks");
    assert!(calls[1].1.contains(&("limit".to_owned(), "4".to_owned())));
    assert_eq!(found.len(), 2);
    assert_eq!(found[1].title, "Three");
    assert_eq!(found[1].position, Some(2));
}

#[tokio::test]
async fn preview_quirk_deezer_album_top_hit_fallback() {
    // No artist loosely matches, so the top hit is used anyway.
    let albums = r#"{"data": [{"id": 7, "title": "C", "artist": {"name": "???"}}]}"#;
    let tracks = r#"{"data": []}"#;
    let http = ScriptPreview::scripted(vec![preply(200, albums), preply(200, tracks)]);
    let client = preview_client(&http);
    assert!(
        client
            .deezer_album_tracks("Brad Sucks", "C", 4)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(http.calls()[1].0, "http://fake/deezer/album/7/tracks");
}

#[test]
fn preview_quirk_names_match_edges() {
    assert!(preview::names_match("Brad Sucks", "brad sucks & co"));
    assert!(preview::names_match("brad sucks & co", "Brad Sucks"));
    assert!(!preview::names_match("", "x"));
    assert!(!preview::names_match("x", ""));
    assert!(!preview::names_match("Nirvana", "Piano Tribute Players"));
    assert_eq!(preview::norm("  A-B_c! "), "abc");
    // Faithful v2 edge: an empty normalized "got" is trivially contained.
    assert!(preview::names_match("x", "!!!"));
}

#[tokio::test]
async fn preview_quirk_itunes_cover_rejected() {
    let body = r#"{"resultCount": 2, "results": [
        {"artistName": "Cover Band", "trackName": "T",
         "collectionName": "C", "previewUrl": "https://audio/cover.m4a"},
        {"artistName": "Brad Sucks", "trackName": "T",
         "collectionName": "C", "previewUrl": "https://audio/real.m4a"}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .itunes_track_preview("Brad Sucks", "T")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.preview_url, "https://audio/real.m4a");
    let calls = http.calls();
    assert_eq!(calls[0].0, "http://fake/itunes");
    assert!(
        calls[0]
            .1
            .contains(&("entity".to_owned(), "song".to_owned()))
    );
    assert!(calls[0].1.contains(&("limit".to_owned(), "5".to_owned())));
}

#[tokio::test]
async fn preview_quirk_itunes_album_sorted_and_verified() {
    let body = r#"{"resultCount": 4, "results": [
        {"artistName": "Brad Sucks", "trackName": "Two",
         "collectionName": "Guess Who's a Mess",
         "previewUrl": "https://audio/2.m4a", "trackNumber": 2},
        {"artistName": "Someone Else", "trackName": "X",
         "collectionName": "Guess Who's a Mess",
         "previewUrl": "https://audio/x.m4a", "trackNumber": 1},
        {"artistName": "Brad Sucks", "trackName": "Wrong Album",
         "collectionName": "Other Record",
         "previewUrl": "https://audio/o.m4a", "trackNumber": 1},
        {"artistName": "Brad Sucks", "trackName": "One",
         "collectionName": "Guess Who's a Mess",
         "previewUrl": "https://audio/1.m4a", "trackNumber": 1}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .itunes_album_tracks("Brad Sucks", "Guess Who's a Mess", 4)
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].title, "One");
    assert_eq!(found[1].title, "Two");
    assert!(
        http.calls()[0]
            .1
            .contains(&("limit".to_owned(), "25".to_owned()))
    );
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
async fn preview_quirk_album_fallback_chain() {
    let albums = r#"{"data": [{"id": 3, "title": "C", "artist": {"name": "A"}}]}"#;
    let tracks = r#"{"data": [
        {"title": "One", "title_short": "", "preview": "https://cdn/1.mp3",
         "track_position": 1, "artist": {"name": "A"}}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, albums), preply(200, tracks)]);
    let (found, provider) = preview_client(&http)
        .get_album_preview_tracks("A", "C", 4)
        .await;
    assert_eq!(found.len(), 1);
    assert_eq!(provider, Some("deezer"));

    let itunes = r#"{"resultCount": 1, "results": [
        {"artistName": "A", "trackName": "One", "collectionName": "C",
         "previewUrl": "https://audio/1.m4a", "trackNumber": 1}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, r#"{"data": []}"#), preply(200, itunes)]);
    let (found, provider) = preview_client(&http)
        .get_album_preview_tracks("A", "C", 4)
        .await;
    assert_eq!(found.len(), 1);
    assert_eq!(provider, Some("itunes"));
}

#[tokio::test]
async fn preview_quirk_artist_top_tracks_filter_dedupe() {
    let body = r#"{"data": [
        {"title": "Hit", "title_short": "", "preview": "https://cdn/1.mp3",
         "artist": {"name": "Brad Sucks"}},
        {"title": "HIT", "title_short": "", "preview": "https://cdn/2.mp3",
         "artist": {"name": "Brad Sucks"}},
        {"title": "Stranger", "title_short": "", "preview": "https://cdn/3.mp3",
         "artist": {"name": "Someone Else"}},
        {"title": "Deep", "title_short": "Deep Cut", "preview": "https://cdn/4.mp3",
         "artist": {"name": "Brad Sucks"}}
    ]}"#;
    let http = ScriptPreview::scripted(vec![preply(200, body)]);
    let found = preview_client(&http)
        .get_artist_top_tracks("Brad Sucks", 5)
        .await;
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].title, "Hit");
    assert_eq!(found[1].title, "Deep Cut");
    assert!(
        http.calls()[0]
            .1
            .contains(&("limit".to_owned(), "10".to_owned()))
    );

    let http = ScriptPreview::scripted(vec![pfault()]);
    assert!(
        preview_client(&http)
            .get_artist_top_tracks("A", 5)
            .await
            .is_empty()
    );
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
async fn lrclib_contract_empty_object_is_unusable_not_a_miss() {
    // `{}` decodes (tolerant wire struct) but fails the identity gate, so it
    // surfaces as an error rather than a quiet miss or an empty candidate.
    let http = ScriptLrclib::scripted(vec![lreply(200, "{}")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    assert_eq!(
        client.get_exact_lyrics("T", "A", "C", 120).await,
        Err(lrclib::FetchError::Unusable)
    );
}

#[tokio::test]
async fn lrclib_quirk_instrumental_found_lyricsless_miss() {
    let body = r#"{"id": 1, "trackName": "T", "artistName": "A",
        "albumName": "C", "duration": 60.0, "instrumental": true,
        "plainLyrics": null, "syncedLyrics": null}"#;
    let http = ScriptLrclib::scripted(vec![lreply(200, body)]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    let found = client.get_exact_lyrics("T", "A", "C", 60).await.unwrap();
    assert!(found.found);

    let body = body.replace("true", "false");
    let http = ScriptLrclib::scripted(vec![lreply(200, &body)]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    let found = client.get_exact_lyrics("T", "A", "C", 60).await.unwrap();
    assert!(!found.found);
    assert_eq!(found.candidate, None);
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

#[test]
fn lrclib_quirk_retry_after_capped() {
    assert_eq!(lrclib::retry_after_secs(Some("45")), 30.0);
    assert_eq!(lrclib::retry_after_secs(Some("12.5")), 12.5);
    assert_eq!(lrclib::retry_after_secs(None), 2.0);
    assert_eq!(lrclib::retry_after_secs(Some("junk")), 2.0);
    assert_eq!(lrclib::retry_after_secs(Some("0")), 2.0);
    assert_eq!(lrclib::retry_after_secs(Some("-3")), 2.0);
}

#[test]
fn lrclib_quirk_apostrophe_signature() {
    // The 2026-08-05 reverify: LRCLIB normalizes typographic apostrophes to
    // ASCII, so the exact signature treats those forms as the same text.
    assert!(lrclib::signature_matches(
        "I Don\u{2019}t Dance",
        "I Don't Dance"
    ));
    assert!(lrclib::signature_matches(
        "\u{2018}quoted\u{2019}",
        "'quoted'"
    ));
    assert!(lrclib::signature_matches("a\u{2bc}b", "a'b"));
    assert!(lrclib::signature_matches("a\u{201b}b", "a'b"));
    assert!(lrclib::signature_matches("  Spaced\tOut ", "spaced out"));
    assert!(!lrclib::signature_matches("Don't Dance", "Don't Sing"));
}

#[tokio::test]
async fn lrclib_quirk_revision_is_sha256_of_body() {
    use sha2::{Digest, Sha256};
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
    let expected = format!("{:x}", Sha256::digest(LRCLIB_HIT.as_bytes()));
    assert_eq!(found.candidate.unwrap().provider_revision, expected);
}

#[tokio::test]
async fn lrclib_quirk_request_shape() {
    let http = ScriptLrclib::scripted(vec![lreply(404, "{}")]);
    let client = lrclib::LrclibClient::with_base(&http, "http://fake");
    let found = client.get_exact_lyrics("T", "A", "C", 120).await.unwrap();
    assert!(!found.found);
    let calls = http.calls();
    assert_eq!(calls[0].0, "http://fake/api/get");
    for pair in [
        ("track_name", "T"),
        ("artist_name", "A"),
        ("album_name", "C"),
        ("duration", "120"),
    ] {
        assert!(
            calls[0].1.contains(&(pair.0.to_owned(), pair.1.to_owned())),
            "missing {pair:?}"
        );
    }
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
async fn archive_contract_list_creator_and_year_digits() {
    let body = format!(
        r#"{{"response": {{"docs": [
            {{"identifier": "x", "title": "T",
              "creator": ["Brad Sucks", "Brad Sucks"],
              "licenseurl": "{ARCHIVE_CC}", "year": "2012"}},
            {{"identifier": "y", "title": "U", "creator": "Solo",
              "licenseurl": "{ARCHIVE_CC}", "year": "2012a"}},
            {{"identifier": "z", "licenseurl": "{ARCHIVE_CC}", "year": 1999}}
        ]}}}}"#
    );
    let http = ScriptArchive::scripted(vec![areply(200, &body)]);
    let items = archive_client(&http)
        .search_audio("A", "T", 12)
        .await
        .unwrap();
    assert_eq!(items[0].creator, "Brad Sucks, Brad Sucks");
    assert_eq!(items[0].year, Some(2012));
    assert_eq!(items[1].creator, "Solo");
    assert_eq!(items[1].year, None);
    assert_eq!(items[2].title, "z");
    assert_eq!(items[2].year, Some(1999));
}

#[tokio::test]
async fn archive_contract_blank_query_makes_no_request() {
    let http = ScriptArchive::scripted(vec![]);
    let items = archive_client(&http)
        .search_audio("", "  ", 12)
        .await
        .unwrap();
    assert!(items.is_empty());
    assert!(http.calls().is_empty());
}

#[tokio::test]
async fn archive_quirk_lucene_escape() {
    assert_eq!(archive::escape_lucene(r#"a\b"c"#), "abc");
    let http = ScriptArchive::scripted(vec![areply(200, r#"{"response": {"docs": []}}"#)]);
    archive_client(&http)
        .search_audio("Brad \"Sucks\"", "T\\", 12)
        .await
        .unwrap();
    let calls = http.calls();
    let query = calls[0]
        .1
        .iter()
        .find(|(key, _)| key == "q")
        .unwrap()
        .1
        .clone();
    assert!(query.contains(r#"creator:"Brad Sucks""#), "query: {query}");
    assert!(query.contains(r#"title:"T""#), "query: {query}");
    assert!(query.contains("mediatype:audio"), "query: {query}");
    assert!(query.contains("licenseurl:[* TO *]"), "query: {query}");
    assert_eq!(calls[0].1.iter().filter(|(k, _)| k == "fl[]").count(), 5);
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
async fn archive_quirk_audio_files_with_track_numbers() {
    let body = format!(
        r#"{{"metadata": {{"licenseurl": "{ARCHIVE_CC}"}}, "files": [
            {{"name": "01.mp3", "format": "VBR MP3", "size": "500",
              "track": "1", "title": "One"}},
            {{"name": "01.flac", "format": "FLAC", "size": 5000, "track": 1}},
            {{"name": "cover.jpg", "format": "JPEG", "size": "10"}}
        ]}}"#
    );
    let http = ScriptArchive::scripted(vec![areply(200, &body)]);
    let (licence, files) = archive_client(&http).get_item_files("x").await.unwrap();
    assert_eq!(licence, ARCHIVE_CC);
    assert_eq!(files.len(), 2);
    let mp3 = files.iter().find(|file| file.name == "01.mp3").unwrap();
    assert_eq!(mp3.size_bytes, 500);
    assert_eq!(mp3.track, Some(1));
    assert_eq!(mp3.title, "One");
    assert_eq!(http.calls()[0].0, "http://fake/metadata/x".to_owned());
}

#[test]
fn archive_quirk_format_map() {
    assert_eq!(archive::extension_for("VBR MP3"), "mp3");
    assert_eq!(archive::extension_for("FLAC"), "flac");
    assert_eq!(archive::extension_for("24bit FLAC"), "flac");
    assert_eq!(archive::extension_for("Ogg Vorbis"), "ogg");
    assert_eq!(archive::extension_for("mp3"), "mp3");
    assert_eq!(archive::extension_for("JPEG"), "");
    assert_eq!(archive::extension_for(""), "");
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

#[test]
fn wikidata_contract_id_and_title_extraction() {
    assert_eq!(
        wikidata::extract_wikidata_id("https://www.wikidata.org/wiki/Q42"),
        Some("Q42".to_owned())
    );
    assert_eq!(
        wikidata::extract_wikidata_id("https://www.wikidata.org/wiki/Q"),
        None
    );
    assert_eq!(
        wikidata::extract_wikidata_id("https://example.com/wiki/Nope"),
        None
    );
    assert_eq!(
        wikidata::extract_wikipedia_title("https://en.wikipedia.org/wiki/Douglas_Adams"),
        Some("Douglas_Adams".to_owned())
    );
    assert_eq!(
        wikidata::extract_wikipedia_title("https://en.wikipedia.org/wiki/"),
        None
    );
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
async fn wikidata_quirk_missing_sitelink_or_page_is_absent() {
    let entity = r#"{"entities": {"Q9": {"sitelinks": {"dewiki": {"title": "X"}}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, entity)]);
    assert_eq!(
        wiki_client(&http)
            .get_bio_extract("https://www.wikidata.org/wiki/Q9", "en")
            .await
            .unwrap(),
        None
    );

    let missing = r#"{"query": {"pages": {"-1": {"pageid": -1}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, missing)]);
    assert_eq!(
        wiki_client(&http)
            .wikipedia_extract("Ghost", "en")
            .await
            .unwrap(),
        None
    );

    let empty = r#"{"query": {"pages": {"1": {"pageid": 1, "extract": ""}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, empty)]);
    assert_eq!(
        wiki_client(&http)
            .wikipedia_extract("Blank", "en")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn wikidata_quirk_image_hop() {
    let claims = r#"{"claims": {"P18": [
        {"mainsnak": {"datavalue": {"value": "Douglas Adams.jpg"}}}
    ]}}"#;
    let commons = r#"{"query": {"pages": {"1": {
        "imageinfo": [{"url": "https://upload.wikimedia.org/x.jpg"}]}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, claims), wreply(200, commons)]);
    let found = wiki_client(&http).get_artist_image("Q42").await.unwrap();
    assert_eq!(found.as_deref(), Some("https://upload.wikimedia.org/x.jpg"));
    let calls = http.calls();
    assert_eq!(calls[0].0, "http://fake/data/w/api.php");
    assert!(
        calls[0]
            .1
            .contains(&("property".to_owned(), "P18".to_owned()))
    );
    assert!(
        calls[1]
            .1
            .contains(&("titles".to_owned(), "File:Douglas Adams.jpg".to_owned()))
    );

    // No claims is absence, not failure.
    let http = ScriptWiki::scripted(vec![wreply(200, r#"{"claims": {}}"#)]);
    assert_eq!(
        wiki_client(&http).get_artist_image("Q1").await.unwrap(),
        None
    );

    // Image info without a URL ends the lookup as absent.
    let claims = r#"{"claims": {"P18": [
        {"mainsnak": {"datavalue": {"value": "X.jpg"}}}
    ]}}"#;
    let commons = r#"{"query": {"pages": {"1": {"imageinfo": [{"url": ""}]}}}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, claims), wreply(200, commons)]);
    assert_eq!(
        wiki_client(&http).get_artist_image("Q1").await.unwrap(),
        None
    );
}

#[test]
fn wikidata_quirk_titles_percent_encoded() {
    assert_eq!(wikidata::percent_encode("Douglas Adams"), "Douglas%20Adams");
    assert_eq!(wikidata::percent_encode("A/C?"), "A%2FC%3F");
    assert_eq!(wikidata::percent_encode("Q42"), "Q42");
}

#[tokio::test]
async fn wikidata_quirk_relation_targets() {
    let claims = r#"{"claims": {"P175": [
        {"mainsnak": {"datavalue": {"value": {"entity-type": "item", "id": "Q1"}}}},
        {"mainsnak": {"datavalue": {"value": "not-an-entity"}}},
        {"mainsnak": {"datavalue": {"value": {"entity-type": "item", "id": "Q2"}}}},
        {"mainsnak": {}}
    ]}}"#;
    let http = ScriptWiki::scripted(vec![wreply(200, claims)]);
    let found = wiki_client(&http)
        .related_entity_ids("Q9", "P175")
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].entity_id, "Q1");
    assert_eq!(found[1].entity_id, "Q2");

    let http = ScriptWiki::scripted(vec![wreply(404, "{}")]);
    assert!(
        wiki_client(&http)
            .related_entity_ids("Q9", "P175")
            .await
            .unwrap()
            .is_empty()
    );
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
