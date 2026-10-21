//! Recorded third-party answers. Each row's fixture decodes through its
//! provider's client, and the same answer without one of its identity fields
//! fails decoding as a payload error. A provider adds a row per identity field
//! when its models land.

use std::sync::Arc;

use droppedneedle_providers::coverart::{DownloadSize, EntityKind};
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
    DeezerArtist,
    WikidataSearch,
    WikidataPictureClaims,
    ItunesAlbum,
    ItunesTrack,
    ItunesAlbumTracks,
    WikidataEntity,
    WikidataClaims,
    WikipediaExtract,
    CommonsImage,
    WikidataRedirect,
    GithubReleases,
    CoverArtRelease,
    CoverArtReleaseGroup,
    AudioDbArtistMbid,
    AudioDbAlbumMbid,
    AudioDbArtistSearch,
    AudioDbAlbumSearch,
    DiscogsRelease(&'static str),
    DiscogsSearch,
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
/// The GitHub releases list the release check reads.
const GITHUB_RELEASES_URL: &str = "http://fake/github/releases";
/// The release group of OK Computer, and the release its listing names as the
/// source of its images. The two listings hold the same two images.
const COVER_ART_RELEASE_GROUP_MBID: &str = "b1392450-e666-3926-a536-22c65f834433";
/// The release the release group's listing names.
const COVER_ART_RELEASE_MBID: &str = "30702389-5c67-4438-9ea0-2351c8de0f1d";
/// Radiohead, and the release group of OK Computer, as TheAudioDB's lookups
/// take them.
const AUDIODB_ARTIST_MBID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const AUDIODB_ALBUM_MBID: &str = "b1392450-e666-3926-a536-22c65f834433";

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
        name: "deezer /artist/{id}, artist id",
        provider: Provider::DeezerArtist,
        body: include_str!("../fixtures/deezer/artist.json"),
        identity: "/id",
    },
    Row {
        name: "wikidata list=search, item id",
        provider: Provider::WikidataSearch,
        body: include_str!("../fixtures/wikidata/search_p434.json"),
        identity: "/query/search/0/title",
    },
    Row {
        name: "wikidata wbgetclaims for every property, claims",
        provider: Provider::WikidataPictureClaims,
        body: include_str!("../fixtures/wikidata/claims_all.json"),
        identity: "/claims",
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
    Row {
        name: "github /repos/{owner}/{repo}/releases, tag name",
        provider: Provider::GithubReleases,
        body: include_str!("../fixtures/github/releases_list.json"),
        identity: "/0/tag_name",
    },
    Row {
        name: "coverartarchive /release/{mbid}, image list",
        provider: Provider::CoverArtRelease,
        body: include_str!("../fixtures/coverartarchive/release_found.json"),
        identity: "/images",
    },
    Row {
        name: "coverartarchive /release/{mbid}, image id",
        provider: Provider::CoverArtRelease,
        body: include_str!("../fixtures/coverartarchive/release_found.json"),
        identity: "/images/0/id",
    },
    Row {
        name: "coverartarchive /release-group/{mbid}, image list",
        provider: Provider::CoverArtReleaseGroup,
        body: include_str!("../fixtures/coverartarchive/release_group_found.json"),
        identity: "/images",
    },
    Row {
        name: "coverartarchive /release-group/{mbid}, image id",
        provider: Provider::CoverArtReleaseGroup,
        body: include_str!("../fixtures/coverartarchive/release_group_found.json"),
        identity: "/images/0/id",
    },
    Row {
        name: "audiodb /artist-mb.php, artist list",
        provider: Provider::AudioDbArtistMbid,
        body: include_str!("../fixtures/audiodb/artist_mb_found.json"),
        identity: "/artists",
    },
    Row {
        name: "audiodb /artist-mb.php, artist id",
        provider: Provider::AudioDbArtistMbid,
        body: include_str!("../fixtures/audiodb/artist_mb_found.json"),
        identity: "/artists/0/idArtist",
    },
    Row {
        name: "audiodb /album-mb.php, album list",
        provider: Provider::AudioDbAlbumMbid,
        body: include_str!("../fixtures/audiodb/album_mb_found.json"),
        identity: "/album",
    },
    Row {
        name: "audiodb /album-mb.php, album id",
        provider: Provider::AudioDbAlbumMbid,
        body: include_str!("../fixtures/audiodb/album_mb_found.json"),
        identity: "/album/0/idAlbum",
    },
    Row {
        name: "audiodb /search.php, artist list",
        provider: Provider::AudioDbArtistSearch,
        body: include_str!("../fixtures/audiodb/search_artist_found.json"),
        identity: "/artists",
    },
    Row {
        name: "audiodb /search.php, artist id",
        provider: Provider::AudioDbArtistSearch,
        body: include_str!("../fixtures/audiodb/search_artist_found.json"),
        identity: "/artists/0/idArtist",
    },
    Row {
        name: "audiodb /searchalbum.php, album list",
        provider: Provider::AudioDbAlbumSearch,
        body: include_str!("../fixtures/audiodb/search_album_found.json"),
        identity: "/album",
    },
    Row {
        name: "audiodb /searchalbum.php, album id",
        provider: Provider::AudioDbAlbumSearch,
        body: include_str!("../fixtures/audiodb/search_album_found.json"),
        identity: "/album/0/idAlbum",
    },
    Row {
        name: "discogs /releases/{id}, release id (7 inch)",
        provider: Provider::DiscogsRelease("249504"),
        body: include_str!("../fixtures/discogs/release_249504.json"),
        identity: "/id",
    },
    Row {
        name: "discogs /releases/{id}, release id (CD with index rows)",
        provider: Provider::DiscogsRelease("3562468"),
        body: include_str!("../fixtures/discogs/release_3562468.json"),
        identity: "/id",
    },
    Row {
        name: "discogs /database/search, hit list",
        provider: Provider::DiscogsSearch,
        body: include_str!("../fixtures/discogs/search_found.json"),
        identity: "/results",
    },
    Row {
        name: "discogs /database/search, hit id",
        provider: Provider::DiscogsSearch,
        body: include_str!("../fixtures/discogs/search_found.json"),
        identity: "/results/0/id",
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
        Provider::DeezerArtist => {
            let picture = deezer(&[("/artist/415", body)])
                .previews
                .deezer_artist_picture(415, PRIORITY)
                .await?;
            Ok(picture.is_some())
        }
        Provider::WikidataSearch => {
            let item = wikidata_answers(&[(WIKIDATA_CLAIMS_URL, body)])
                .wikidata
                .item_for_musicbrainz_artist("5b11f4ce-a62d-471e-81fc-a69a8278c7da", PRIORITY)
                .await?;
            Ok(item.is_some())
        }
        Provider::WikidataPictureClaims => {
            let ids = wikidata_answers(&[(WIKIDATA_CLAIMS_URL, body)])
                .wikidata
                .artist_picture_ids("Q11649", PRIORITY)
                .await?;
            Ok(ids.is_some_and(|ids| ids.deezer_artist.is_some()))
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
        Provider::GithubReleases => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(GITHUB_RELEASES_URL, 200, &[], body.as_bytes().to_vec());
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    github_releases: GITHUB_RELEASES_URL.to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let releases = providers.github.fetch(PRIORITY).await?;
            Ok(!releases.is_empty())
        }
        Provider::CoverArtRelease => {
            cover_art(EntityKind::Release, COVER_ART_RELEASE_MBID, body).await
        }
        Provider::CoverArtReleaseGroup => {
            cover_art(EntityKind::ReleaseGroup, COVER_ART_RELEASE_GROUP_MBID, body).await
        }
        Provider::AudioDbArtistMbid => {
            let providers = audiodb("artist-mb.php", body);
            let artist = providers
                .audiodb
                .artist_by_mbid("", AUDIODB_ARTIST_MBID, PRIORITY)
                .await?;
            Ok(artist.is_some())
        }
        Provider::AudioDbAlbumMbid => {
            let providers = audiodb("album-mb.php", body);
            let album = providers
                .audiodb
                .album_by_mbid("", AUDIODB_ALBUM_MBID, PRIORITY)
                .await?;
            Ok(album.is_some())
        }
        Provider::AudioDbArtistSearch => {
            let providers = audiodb("search.php", body);
            let artist = providers
                .audiodb
                .search_artist("", "Radiohead", PRIORITY)
                .await?;
            Ok(artist.is_some())
        }
        Provider::AudioDbAlbumSearch => {
            let providers = audiodb("searchalbum.php", body);
            let album = providers
                .audiodb
                .search_album("", "Radiohead", "OK Computer", PRIORITY)
                .await?;
            Ok(album.is_some())
        }
        Provider::DiscogsRelease(release_id) => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                &format!("http://fake/discogs/releases/{release_id}"),
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    discogs: "http://fake/discogs".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let release = providers
                .discogs
                .get_release(release_id, 1.0, PRIORITY)
                .await?;
            Ok(release.is_some())
        }
        Provider::DiscogsSearch => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/discogs/database/search",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    discogs: "http://fake/discogs".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let candidates = providers
                .discogs
                .search_releases("Nirvana Nevermind", 5, 1.0, PRIORITY)
                .await?;
            Ok(!candidates.is_empty())
        }
    }
}

/// Serves `body` from the archive's listing address for the entity and reads
/// it through the archive's client.
async fn cover_art(entity: EntityKind, mbid: &str, body: &str) -> Result<bool, ProviderError> {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("http://fake/coverart/{}/{mbid}", entity.path()),
        200,
        &[],
        body.as_bytes().to_vec(),
    );
    let providers = Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            coverart: "http://fake/coverart".to_owned(),
            ..ProviderEndpoints::closed()
        },
    );
    let images = providers
        .coverart
        .list_artwork(entity, mbid, DownloadSize::Full, PRIORITY)
        .await?;
    Ok(!images.is_empty())
}

/// Serves `body` from TheAudioDB's free-key address for `endpoint` and returns
/// the providers that read it.
fn audiodb(endpoint: &str, body: &str) -> Providers {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("http://fake/audiodb/123/{endpoint}"),
        200,
        &[],
        body.as_bytes().to_vec(),
    );
    Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            audiodb: "http://fake/audiodb".to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
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
