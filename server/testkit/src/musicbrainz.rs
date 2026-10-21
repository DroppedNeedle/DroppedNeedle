//! In-repo mock MusicBrainz (`/ws/2`) that answers only with recorded
//! documents.
//!
//! A request is answered when its path, its `inc` set and its one
//! distinguishing query parameter match a recording. Any other request
//! gets MusicBrainz's recorded not-found answer, so a missing recording
//! shows up as an absence, never as a document the service did not send.
//! Other parameters (`fmt`, `limit`, `offset`, the search text) are not
//! compared: a search answers with its recording whatever was searched.
//!
//! Sources of the recordings, beside each row below:
//! - `tests/fixtures/catalog/mb_*.json` were added with the catalog page
//!   tests on 2026-10-06 as recorded payloads; they carry no note of when
//!   or from which server version. Each is served for the request the
//!   search, artist or album page sends, whose includes the document
//!   carries.
//! - `tests/fixtures/musicbrainz/release_relations_*`, `release_names_*`
//!   and `artist_merged_*` were recorded read-only from the BrainzMash
//!   mirror on 2026-10-15 and 2026-10-17; the README beside them names
//!   each request.
//! - `tests/fixtures/musicbrainz/spotify_*` are written by hand in the
//!   shapes of the recorded release search and lookup, for the albums of
//!   the Spotify mock's playlist.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;

use axum::{
    extract::Query,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

/// The Radiohead artist id the catalog recordings describe.
const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

/// MusicBrainz's not-found body, recorded from BrainzMash on 2026-10-17
/// for `artist/5b0c7e7a-3f0e-4d4a-9a51-2f6c1d8e9b30`.
const NOT_FOUND: &str = include_str!("../../tests/fixtures/musicbrainz/artist_merged_missing.json");

/// One recorded answer and the request it answers.
struct Recording {
    /// Path under `/ws/2`.
    path: &'static str,
    /// The include set the request must carry, compared as a set.
    inc: &'static [&'static str],
    /// A query parameter the request must carry, with the value it must
    /// have or `None` for any value. `None` for a lookup by id.
    param: Option<(&'static str, Option<&'static str>)>,
    body: &'static str,
}

const RECORDINGS: &[Recording] = &[
    // catalog/mb_search_artists.json: artist search, and the Mirror
    // connection test (`artist?query=test&limit=1`).
    Recording {
        path: "/artist",
        inc: &[],
        param: Some(("query", None)),
        body: include_str!("../../tests/fixtures/catalog/mb_search_artists.json"),
    },
    // catalog/mb_search_release_groups.json: album search.
    Recording {
        path: "/release-group",
        inc: &[],
        param: Some(("query", None)),
        body: include_str!("../../tests/fixtures/catalog/mb_search_release_groups.json"),
    },
    // catalog/mb_artist_release_groups.json: Radiohead's release groups,
    // the artist page and an album's "More by".
    Recording {
        path: "/release-group",
        inc: &["artist-credits"],
        param: Some(("artist", Some(RADIOHEAD))),
        body: include_str!("../../tests/fixtures/catalog/mb_artist_release_groups.json"),
    },
    // catalog/mb_artist.json: Radiohead, as the artist page and search
    // ask for it.
    Recording {
        path: "/artist/a74b1b7f-71a5-4011-9441-d0b5e4122711",
        inc: &["aliases", "tags", "url-rels"],
        param: None,
        body: include_str!("../../tests/fixtures/catalog/mb_artist.json"),
    },
    // catalog/mb_release_group.json: OK Computer, as the album page asks
    // for it.
    Recording {
        path: "/release-group/b1392450-e666-3926-a536-22c65f834433",
        inc: &["artist-credits", "media", "releases", "url-rels"],
        param: None,
        body: include_str!("../../tests/fixtures/catalog/mb_release_group.json"),
    },
    // catalog/mb_release_cd.json: the OK Computer CD, as the album page
    // asks for each release of the group.
    Recording {
        path: "/release/6b1c7d8a-5f0e-4cd3-8e8f-0c1d3a7b9f65",
        inc: &["labels", "recordings", "url-rels"],
        param: None,
        body: include_str!("../../tests/fixtures/catalog/mb_release_cd.json"),
    },
    // musicbrainz/release_relations_classical.json (2026-10-15).
    Recording {
        path: "/release/7ce871a0-34b2-40e0-a77e-39b1268f1f61",
        inc: &[
            "artist-rels",
            "recording-level-rels",
            "recordings",
            "work-level-rels",
            "work-rels",
        ],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/release_relations_classical.json"),
    },
    // musicbrainz/release_relations_pop.json (2026-10-15).
    Recording {
        path: "/release/863644bd-df97-3259-a17c-e0d51a6ffcc0",
        inc: &[
            "artist-rels",
            "recording-level-rels",
            "recordings",
            "work-level-rels",
            "work-rels",
        ],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/release_relations_pop.json"),
    },
    // musicbrainz/release_names_sakamoto.json (2026-10-15).
    Recording {
        path: "/release/82316878-b6ff-44da-8be3-96c253d36cc7",
        inc: &["aliases", "artist-credits", "recordings"],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/release_names_sakamoto.json"),
    },
    // musicbrainz/release_names_utada.json (2026-10-15).
    Recording {
        path: "/release/d4ed5eba-fa9d-4325-8279-093fccdb4a45",
        inc: &["aliases", "artist-credits", "recordings"],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/release_names_utada.json"),
    },
    // musicbrainz/release_names_classical.json (2026-10-15).
    Recording {
        path: "/release/7ce871a0-34b2-40e0-a77e-39b1268f1f61",
        inc: &[
            "aliases",
            "artist-credits",
            "artist-rels",
            "recording-level-rels",
            "recordings",
            "work-level-rels",
            "work-rels",
        ],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/release_names_classical.json"),
    },
    // musicbrainz/artist_merged_canonical.json (2026-10-17).
    Recording {
        path: "/artist/ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea",
        inc: &[],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/artist_merged_canonical.json"),
    },
    // musicbrainz/spotify_*.json (hand-written): the two albums of the
    // Spotify mock's playlist, searched and proven by ISRC. The searches
    // answer only their own query, so no other release search finds them.
    Recording {
        path: "/release",
        inc: &[],
        param: Some((
            "query",
            Some(r#"release:"Neon Meridian" AND artist:"Aurora Current""#),
        )),
        body: include_str!("../../tests/fixtures/musicbrainz/spotify_search_neon.json"),
    },
    Recording {
        path: "/release/5907a1b0-0000-4000-8000-000000000a01",
        inc: &["isrcs", "recordings", "release-groups"],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/spotify_release_neon.json"),
    },
    Recording {
        path: "/release",
        inc: &[],
        param: Some((
            "query",
            Some(r#"release:"Glass Tides" AND artist:"Boreal Static""#),
        )),
        body: include_str!("../../tests/fixtures/musicbrainz/spotify_search_glass.json"),
    },
    Recording {
        path: "/release/5907a1b0-0000-4000-8000-000000000a02",
        inc: &["isrcs", "recordings", "release-groups"],
        param: None,
        body: include_str!("../../tests/fixtures/musicbrainz/spotify_release_glass.json"),
    },
];

/// The recording for a request, if one was made for it.
fn find(path: &str, query: &HashMap<String, String>) -> Option<&'static str> {
    // `inc` arrives joined with `+`, which form decoding turns into spaces.
    let inc: BTreeSet<&str> = query
        .get("inc")
        .map(|value| {
            value
                .split([' ', '+'])
                .filter(|part| !part.is_empty())
                .collect()
        })
        .unwrap_or_default();
    RECORDINGS
        .iter()
        .find(|recording| {
            recording.path == path
                && recording.inc.iter().copied().collect::<BTreeSet<_>>() == inc
                && match recording.param {
                    Some((name, want)) => query
                        .get(name)
                        .is_some_and(|value| want.is_none_or(|want| want == value)),
                    None => !query.contains_key("query") && !query.contains_key("artist"),
                }
        })
        .map(|recording| recording.body)
}

async fn answer(uri: Uri, Query(query): Query<HashMap<String, String>>) -> Response {
    let path = uri.path().strip_prefix("/ws/2").unwrap_or_default();
    let (status, body) = match find(path, &query) {
        Some(body) => (StatusCode::OK, body),
        None => {
            eprintln!("mock musicbrainz: no recording for {uri}");
            (StatusCode::NOT_FOUND, NOT_FOUND)
        }
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
        .into_response()
}

fn router() -> axum::Router {
    axum::Router::new().fallback(answer)
}

/// A running mock MusicBrainz. Point a client at [`Self::base_url`].
pub struct MockMusicBrainz {
    addr: SocketAddr,
}

impl MockMusicBrainz {
    /// Serve the mock on an ephemeral loopback port.
    pub async fn start() -> Result<Self, String> {
        Self::start_at(SocketAddr::from(([127, 0, 0, 1], 0))).await
    }

    /// Serve the mock on the given address.
    pub async fn start_at(addr: SocketAddr) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|error| format!("mock musicbrainz binds {addr}: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("mock musicbrainz has an address: {error}"))?;
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router()).await {
                eprintln!("mock musicbrainz stopped: {error}");
            }
        });
        Ok(Self { addr })
    }

    /// The `/ws/2` base a client is configured with.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}/ws/2", self.addr)
    }
}
