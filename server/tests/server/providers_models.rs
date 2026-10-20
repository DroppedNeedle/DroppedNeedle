//! Recorded third-party answers. Each row's fixture decodes through its
//! provider's client, and the same answer without one of its identity fields
//! fails decoding as a payload error. A provider adds a row per identity field
//! when its models land.

use std::sync::Arc;

use droppedneedle_providers::executor::ScriptedWire;
use droppedneedle_providers::geocoding::DEFAULT_CITY_COUNT;
use droppedneedle_providers::{
    PayloadKind, ProviderEndpoints, ProviderError, Providers, RequestPriority,
};

const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// The provider whose client reads a row's answer.
#[derive(Clone, Copy)]
enum Provider {
    Lrclib,
    Geocoding,
    DeezerTrack,
    DeezerAlbumSearch,
    DeezerAlbumTracks,
    ItunesAlbum,
    ItunesTrack,
    ItunesAlbumTracks,
    WikidataEntity,
    WikidataClaims,
    WikipediaExtract,
    CommonsImage,
    WikidataRedirect,
}

/// The album the Deezer album search fixture names first, and its track list.
const DEEZER_ALBUM_PATH: &str = "/album/1007321681/tracks";
const DEEZER_ALBUM_SEARCH_BODY: &str = include_str!("../fixtures/deezer/search_album.json");
const DEEZER_ALBUM_TRACKS_BODY: &str = include_str!("../fixtures/deezer/album_tracks.json");

/// The origins the Wikidata rows answer from, and the claim and image info
/// the Commons row reads before its own answer.
const WIKIDATA_ENTITY_URL: &str = "http://fake/wikidata/wiki/Special:EntityData/Q44190.json";
const WIKIDATA_CLAIMS_URL: &str = "http://fake/wikidata/w/api.php";
const WIKIDATA_CLAIMS_BODY: &str = include_str!("../fixtures/wikidata/claims_p18_found.json");
const WIKIPEDIA_EXTRACT_URL: &str = "http://fake/en.wiki/w/api.php";
const COMMONS_IMAGE_URL: &str = "http://fake/commons/w/api.php";
/// A merged item, asked for by its own id: the answer holds its target, Q1.
const WIKIDATA_REDIRECT_URL: &str = "http://fake/wikidata/wiki/Special:EntityData/Q13165295.json";

/// One recorded answer.
struct Row {
    name: &'static str,
    provider: Provider,
    /// The recorded body, as the provider sent it.
    body: &'static str,
    /// A JSON Pointer to the field that identifies the answer. Without it,
    /// decoding fails.
    identity: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        name: "lrclib /api/get",
        provider: Provider::Lrclib,
        body: include_str!("../fixtures/lrclib/get_found.json"),
        identity: "/id",
    },
    Row {
        name: "open-meteo /v1/search, latitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        identity: "/results/0/latitude",
    },
    Row {
        name: "open-meteo /v1/search, longitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        identity: "/results/0/longitude",
    },
    Row {
        name: "deezer /search, track id",
        provider: Provider::DeezerTrack,
        body: include_str!("../fixtures/deezer/search_artist.json"),
        identity: "/data/0/id",
    },
    Row {
        name: "deezer /search/album, album id",
        provider: Provider::DeezerAlbumSearch,
        body: DEEZER_ALBUM_SEARCH_BODY,
        identity: "/data/0/id",
    },
    Row {
        name: "deezer /album/{id}/tracks, track id",
        provider: Provider::DeezerAlbumTracks,
        body: DEEZER_ALBUM_TRACKS_BODY,
        identity: "/data/0/id",
    },
    Row {
        name: "itunes /search album, collection id",
        provider: Provider::ItunesAlbum,
        body: include_str!("../fixtures/itunes/search_album.json"),
        identity: "/results/0/collectionId",
    },
    Row {
        name: "itunes /search song, track id",
        provider: Provider::ItunesTrack,
        body: include_str!("../fixtures/itunes/search_track.json"),
        identity: "/results/0/trackId",
    },
    Row {
        name: "itunes /search song, album tracks, track id",
        provider: Provider::ItunesAlbumTracks,
        body: include_str!("../fixtures/itunes/search_album_tracks.json"),
        identity: "/results/0/trackId",
    },
    Row {
        name: "wikidata Special:EntityData, entity id",
        provider: Provider::WikidataEntity,
        body: include_str!("../fixtures/wikidata/entity_found.json"),
        identity: "/entities/Q44190/id",
    },
    Row {
        name: "wikidata Special:EntityData, the entity asked for",
        provider: Provider::WikidataEntity,
        body: include_str!("../fixtures/wikidata/entity_found.json"),
        identity: "/entities/Q44190",
    },
    Row {
        name: "wikidata wbgetclaims, P18 file name",
        provider: Provider::WikidataClaims,
        body: WIKIDATA_CLAIMS_BODY,
        identity: "/claims/P18/0/mainsnak/datavalue/value",
    },
    Row {
        name: "en.wikipedia query extracts, page id",
        provider: Provider::WikipediaExtract,
        body: include_str!("../fixtures/wikidata/extract_found.json"),
        identity: "/query/pages/38252/pageid",
    },
    Row {
        name: "commons imageinfo, file url",
        provider: Provider::CommonsImage,
        body: include_str!("../fixtures/wikidata/commons_imageinfo_found.json"),
        identity: "/query/pages/179695193/imageinfo/0/url",
    },
    Row {
        name: "wikidata Special:EntityData, redirected id, the target entity",
        provider: Provider::WikidataRedirect,
        body: include_str!("../fixtures/wikidata/entity_redirect.json"),
        identity: "/entities/Q1",
    },
];

/// Serves `body` from the provider's address and reads it through the
/// provider's client: whether the answer is usable, or the typed failure.
async fn read(provider: Provider, body: &str) -> Result<bool, ProviderError> {
    match provider {
        Provider::Lrclib => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/lrclib/api/get",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    lrclib: "http://fake/lrclib".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let lookup = providers
                .lrclib
                .get_exact_lyrics(
                    "Bohemian Rhapsody",
                    "Queen",
                    "A Night at the Opera",
                    355,
                    PRIORITY,
                )
                .await?;
            Ok(lookup.found)
        }
        Provider::Geocoding => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/geocoding/v1/search",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    geocoding: "http://fake/geocoding/v1/search".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let cities = providers
                .geocoding
                .search_cities("Liverpool", DEFAULT_CITY_COUNT, PRIORITY)
                .await?;
            Ok(!cities.is_empty())
        }
        Provider::DeezerTrack => {
            let providers = deezer(&[("/search", body)]);
            let found = providers
                .previews
                .deezer_track_preview("Queen", "Bohemian Rhapsody", PRIORITY)
                .await?;
            Ok(found.is_some())
        }
        Provider::DeezerAlbumSearch => {
            let providers = deezer(&[
                ("/search/album", body),
                (DEEZER_ALBUM_PATH, DEEZER_ALBUM_TRACKS_BODY),
            ]);
            let tracks = providers
                .previews
                .deezer_album_tracks("Queen", "A Night At The Opera", 4, PRIORITY)
                .await?;
            Ok(!tracks.is_empty())
        }
        Provider::DeezerAlbumTracks => {
            let providers = deezer(&[
                ("/search/album", DEEZER_ALBUM_SEARCH_BODY),
                (DEEZER_ALBUM_PATH, body),
            ]);
            let tracks = providers
                .previews
                .deezer_album_tracks("Queen", "A Night At The Opera", 4, PRIORITY)
                .await?;
            Ok(!tracks.is_empty())
        }
        Provider::ItunesAlbum => {
            let found = itunes(body)
                .itunes
                .find_album("Nirvana", "Nevermind", "US", PRIORITY)
                .await?;
            Ok(found.is_some())
        }
        Provider::ItunesTrack => {
            let found = itunes(body)
                .previews
                .itunes_track_preview("Nirvana", "Smells Like Teen Spirit", PRIORITY)
                .await?;
            Ok(found.is_some())
        }
        Provider::ItunesAlbumTracks => {
            let tracks = itunes(body)
                .previews
                .itunes_album_tracks("Nirvana", "Nevermind", 4, PRIORITY)
                .await?;
            Ok(!tracks.is_empty())
        }
        Provider::WikidataEntity => {
            let providers = wikidata_answers(&[(WIKIDATA_ENTITY_URL, body)]);
            let title = providers
                .wikidata
                .wikipedia_title_from_wikidata("Q44190", "en", PRIORITY)
                .await?;
            Ok(title.is_some())
        }
        Provider::WikidataClaims => {
            let providers = wikidata_answers(&[(WIKIDATA_CLAIMS_URL, body)]);
            let file = providers
                .wikidata
                .claim_string("Q44190", "P18", PRIORITY)
                .await?;
            Ok(file.is_some())
        }
        Provider::WikipediaExtract => {
            let providers = wikidata_answers(&[(WIKIPEDIA_EXTRACT_URL, body)]);
            let extract = providers
                .wikidata
                .wikipedia_extract("Radiohead", "en", PRIORITY)
                .await?;
            Ok(extract.is_some())
        }
        Provider::CommonsImage => {
            let providers = wikidata_answers(&[
                (WIKIDATA_CLAIMS_URL, WIKIDATA_CLAIMS_BODY),
                (COMMONS_IMAGE_URL, body),
            ]);
            let image = providers
                .wikidata
                .get_artist_image("Q44190", PRIORITY)
                .await?;
            Ok(image.is_some())
        }
        Provider::WikidataRedirect => {
            let providers = wikidata_answers(&[(WIKIDATA_REDIRECT_URL, body)]);
            let title = providers
                .wikidata
                .wikipedia_title_from_wikidata("Q13165295", "en", PRIORITY)
                .await?;
            Ok(title.is_some())
        }
    }
}

/// Serves each `(path, body)` from the Deezer origin and returns the providers
/// that read them.
fn deezer(answers: &[(&str, &str)]) -> Providers {
    let wire = Arc::new(ScriptedWire::new());
    for (path, body) in answers {
        wire.answer(
            &format!("http://fake/deezer{path}"),
            200,
            &[],
            body.as_bytes().to_vec(),
        );
    }
    Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            deezer: "http://fake/deezer".to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
}

/// Serves `body` from the iTunes search address and returns the providers that
/// read it.
fn itunes(body: &str) -> Providers {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        "http://fake/itunes/search",
        200,
        &[],
        body.as_bytes().to_vec(),
    );
    Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            itunes: "http://fake/itunes/search".to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
}

/// Serves each `(url, body)` from the Wikidata, Wikipedia and Commons origins
/// and returns the providers that read them.
fn wikidata_answers(answers: &[(&str, &str)]) -> Providers {
    let wire = Arc::new(ScriptedWire::new());
    for (url, body) in answers {
        wire.answer(url, 200, &[], body.as_bytes().to_vec());
    }
    Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            wikidata: "http://fake/wikidata".to_owned(),
            wikipedia: "http://fake/{lang}.wiki".to_owned(),
            commons: "http://fake/commons".to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
}

#[tokio::test]
async fn recorded_answers_decode_and_need_their_identity() {
    for row in ROWS {
        let usable = read(row.provider, row.body).await;
        assert!(matches!(usable, Ok(true)), "{}: {usable:?}", row.name);

        let mut answer: serde_json::Value =
            serde_json::from_str(row.body).expect("recorded body parses");
        let (parent, field) = row
            .identity
            .rsplit_once('/')
            .expect("identity is a JSON Pointer");
        answer
            .pointer_mut(parent)
            .and_then(serde_json::Value::as_object_mut)
            .expect("the identity's parent is an object")
            .remove(field)
            .expect("the recorded body carries its identity");
        let error = read(row.provider, &answer.to_string())
            .await
            .expect_err(row.name);
        assert!(
            matches!(
                error,
                ProviderError::Payload {
                    kind: PayloadKind::Decode,
                    ..
                }
            ),
            "{}: without `{}` the answer must fail decoding: {error:?}",
            row.name,
            row.identity
        );
    }
}
