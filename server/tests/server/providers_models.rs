//! Recorded third-party answers. Each row's fixture decodes through its
//! provider's client, and the same answer without one of its identity fields
//! fails decoding as a payload error. A provider adds a row per identity field
//! when its models land.

use std::sync::Arc;

use droppedneedle_providers::coverart::{DownloadSize, EntityKind};
use droppedneedle_providers::executor::ScriptedWire;
use droppedneedle_providers::geocoding::DEFAULT_CITY_COUNT;
use droppedneedle_providers::lastfm::LastFmCredentials;
use droppedneedle_providers::musicbrainz::{self, Criticality, MbSource};
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
    SkiddleArtists,
    SkiddleEvents,
    DiscogsRelease(&'static str),
    DiscogsSearch,
    MusicBrainzRelease(&'static str, &'static [&'static str]),
    MusicBrainzReleaseGroup(&'static str, &'static [&'static str]),
    MusicBrainzArtist(&'static str, &'static [&'static str]),
    MusicBrainzRecording(&'static str, &'static [&'static str]),
    MusicBrainzUrl(&'static str, &'static [&'static str]),
    MusicBrainzArtistSearch(&'static str),
    MusicBrainzReleaseGroupSearch(&'static str),
    MusicBrainzReleaseSearch(&'static str, &'static str),
    MusicBrainzRecordingSearch(&'static str),
    MusicBrainzBrowse(&'static str),
    TicketmasterAttractions,
    TicketmasterEvents,
    LastFm(LastFmRead),
}

/// The Last.fm read a row's answer goes through.
#[derive(Clone, Copy)]
enum LastFmRead {
    ArtistInfo,
    AlbumInfo,
    Similar,
    ArtistTopTracks,
    ArtistTopAlbums,
    ArtistTopTags,
    AlbumTopTags,
    ChartTopArtists,
    TagTopArtists,
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
/// The releases and the artist of the recorded MusicBrainz answers, and the
/// include sets each was read with.
const MB_CLASSICAL_RELEASE_MBID: &str = "7ce871a0-34b2-40e0-a77e-39b1268f1f61";
const MB_POP_RELEASE_MBID: &str = "863644bd-df97-3259-a17c-e0d51a6ffcc0";
const MB_SAKAMOTO_RELEASE_MBID: &str = "82316878-b6ff-44da-8be3-96c253d36cc7";
const MB_UTADA_RELEASE_MBID: &str = "d4ed5eba-fa9d-4325-8279-093fccdb4a45";
const MB_MERGED_ARTIST_MBID: &str = "ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea";
const MB_RECORDING_MBID: &str = "33ea7e59-3691-4414-92d3-6bce48d5d9cc";
const MB_OK_COMPUTER_GROUP_MBID: &str = "b1392450-e666-3926-a536-22c65f834433";
const MB_RADIOHEAD_MBID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const MB_RELEASE_INCLUDES: &[&str] = &[
    "artist-credits",
    "genres",
    "isrcs",
    "labels",
    "recordings",
    "release-groups",
    "url-rels",
];
const MB_RELEASE_GROUP_INCLUDES: &[&str] =
    &["artist-credits", "media", "releases", "tags", "url-rels"];
const MB_ARTIST_INCLUDES: &[&str] = &["aliases", "tags", "url-rels"];
const MB_DISCOGS_RELEASE_URL: &str = "https://www.discogs.com/release/249504";
const MB_URL_INCLUDES: &[&str] = &["release-group-rels", "release-rels"];
const MB_RELATION_INCLUDES: &[&str] = &[
    "artist-rels",
    "recording-level-rels",
    "recordings",
    "work-level-rels",
    "work-rels",
];
const MB_NAME_INCLUDES: &[&str] = &["aliases", "artist-credits", "recordings"];
const MB_NAME_AND_RELATION_INCLUDES: &[&str] = &[
    "aliases",
    "artist-credits",
    "artist-rels",
    "recording-level-rels",
    "recordings",
    "work-level-rels",
    "work-rels",
];

/// One recorded answer.
struct Row {
    name: &'static str,
    provider: Provider,
    /// The recorded body, as the provider sent it.
    body: &'static str,
    /// A JSON Pointer to the field that identifies the answer. Without it,
    /// decoding fails.
    identity: &'static str,
    /// The payload kind a missing identity fails as. An answer read whole
    /// fails as a decode error; a search page reports a hit without its id as
    /// a contract break.
    kind: PayloadKind,
}

const ROWS: &[Row] = &[
    Row {
        name: "lrclib /api/get",
        provider: Provider::Lrclib,
        body: include_str!("../fixtures/lrclib/get_found.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "open-meteo /v1/search, latitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/latitude",
    },
    Row {
        name: "open-meteo /v1/search, longitude",
        provider: Provider::Geocoding,
        body: include_str!("../fixtures/geocoding/search_liverpool.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/longitude",
    },
    Row {
        name: "deezer /search, track id",
        provider: Provider::DeezerTrack,
        body: include_str!("../fixtures/deezer/search_artist.json"),
        kind: PayloadKind::Decode,
        identity: "/data/0/id",
    },
    Row {
        name: "deezer /search/album, album id",
        provider: Provider::DeezerAlbumSearch,
        body: DEEZER_ALBUM_SEARCH_BODY,
        kind: PayloadKind::Decode,
        identity: "/data/0/id",
    },
    Row {
        name: "deezer /album/{id}/tracks, track id",
        provider: Provider::DeezerAlbumTracks,
        body: DEEZER_ALBUM_TRACKS_BODY,
        kind: PayloadKind::Decode,
        identity: "/data/0/id",
    },
    Row {
        name: "deezer /artist/{id}, artist id",
        provider: Provider::DeezerArtist,
        body: include_str!("../fixtures/deezer/artist.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "wikidata list=search, item id",
        provider: Provider::WikidataSearch,
        body: include_str!("../fixtures/wikidata/search_p434.json"),
        kind: PayloadKind::Decode,
        identity: "/query/search/0/title",
    },
    Row {
        name: "wikidata wbgetclaims for every property, claims",
        provider: Provider::WikidataPictureClaims,
        body: include_str!("../fixtures/wikidata/claims_all.json"),
        kind: PayloadKind::Decode,
        identity: "/claims",
    },
    Row {
        name: "itunes /search album, collection id",
        provider: Provider::ItunesAlbum,
        body: include_str!("../fixtures/itunes/search_album.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/collectionId",
    },
    Row {
        name: "itunes /search song, track id",
        provider: Provider::ItunesTrack,
        body: include_str!("../fixtures/itunes/search_track.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/trackId",
    },
    Row {
        name: "itunes /search song, album tracks, track id",
        provider: Provider::ItunesAlbumTracks,
        body: include_str!("../fixtures/itunes/search_album_tracks.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/trackId",
    },
    Row {
        name: "wikidata Special:EntityData, entity id",
        provider: Provider::WikidataEntity,
        body: include_str!("../fixtures/wikidata/entity_found.json"),
        kind: PayloadKind::Decode,
        identity: "/entities/Q44190/id",
    },
    Row {
        name: "wikidata Special:EntityData, the entity asked for",
        provider: Provider::WikidataEntity,
        body: include_str!("../fixtures/wikidata/entity_found.json"),
        kind: PayloadKind::Decode,
        identity: "/entities/Q44190",
    },
    Row {
        name: "wikidata wbgetclaims, P18 file name",
        provider: Provider::WikidataClaims,
        body: WIKIDATA_CLAIMS_BODY,
        kind: PayloadKind::Decode,
        identity: "/claims/P18/0/mainsnak/datavalue/value",
    },
    Row {
        name: "en.wikipedia query extracts, page id",
        provider: Provider::WikipediaExtract,
        body: include_str!("../fixtures/wikidata/extract_found.json"),
        kind: PayloadKind::Decode,
        identity: "/query/pages/38252/pageid",
    },
    Row {
        name: "commons imageinfo, file url",
        provider: Provider::CommonsImage,
        body: include_str!("../fixtures/wikidata/commons_imageinfo_found.json"),
        kind: PayloadKind::Decode,
        identity: "/query/pages/179695193/imageinfo/0/url",
    },
    Row {
        name: "commons imageinfo with author and licence, file url",
        provider: Provider::CommonsImage,
        body: include_str!("../fixtures/wikidata/commons_imageinfo_credit.json"),
        kind: PayloadKind::Decode,
        identity: "/query/pages/179695193/imageinfo/0/url",
    },
    Row {
        name: "wikidata Special:EntityData, redirected id, the target entity",
        provider: Provider::WikidataRedirect,
        body: include_str!("../fixtures/wikidata/entity_redirect.json"),
        kind: PayloadKind::Decode,
        identity: "/entities/Q1",
    },
    Row {
        name: "github /repos/{owner}/{repo}/releases, tag name",
        provider: Provider::GithubReleases,
        body: include_str!("../fixtures/github/releases_list.json"),
        kind: PayloadKind::Decode,
        identity: "/0/tag_name",
    },
    Row {
        name: "coverartarchive /release/{mbid}, image list",
        provider: Provider::CoverArtRelease,
        body: include_str!("../fixtures/coverartarchive/release_found.json"),
        kind: PayloadKind::Decode,
        identity: "/images",
    },
    Row {
        name: "coverartarchive /release/{mbid}, image id",
        provider: Provider::CoverArtRelease,
        body: include_str!("../fixtures/coverartarchive/release_found.json"),
        kind: PayloadKind::Decode,
        identity: "/images/0/id",
    },
    Row {
        name: "coverartarchive /release-group/{mbid}, image list",
        provider: Provider::CoverArtReleaseGroup,
        body: include_str!("../fixtures/coverartarchive/release_group_found.json"),
        kind: PayloadKind::Decode,
        identity: "/images",
    },
    Row {
        name: "coverartarchive /release-group/{mbid}, image id",
        provider: Provider::CoverArtReleaseGroup,
        body: include_str!("../fixtures/coverartarchive/release_group_found.json"),
        kind: PayloadKind::Decode,
        identity: "/images/0/id",
    },
    Row {
        name: "audiodb /artist-mb.php, artist list",
        provider: Provider::AudioDbArtistMbid,
        body: include_str!("../fixtures/audiodb/artist_mb_found.json"),
        kind: PayloadKind::Decode,
        identity: "/artists",
    },
    Row {
        name: "audiodb /artist-mb.php, artist id",
        provider: Provider::AudioDbArtistMbid,
        body: include_str!("../fixtures/audiodb/artist_mb_found.json"),
        kind: PayloadKind::Decode,
        identity: "/artists/0/idArtist",
    },
    Row {
        name: "audiodb /album-mb.php, album list",
        provider: Provider::AudioDbAlbumMbid,
        body: include_str!("../fixtures/audiodb/album_mb_found.json"),
        kind: PayloadKind::Decode,
        identity: "/album",
    },
    Row {
        name: "audiodb /album-mb.php, album id",
        provider: Provider::AudioDbAlbumMbid,
        body: include_str!("../fixtures/audiodb/album_mb_found.json"),
        kind: PayloadKind::Decode,
        identity: "/album/0/idAlbum",
    },
    Row {
        name: "audiodb /search.php, artist list",
        provider: Provider::AudioDbArtistSearch,
        body: include_str!("../fixtures/audiodb/search_artist_found.json"),
        kind: PayloadKind::Decode,
        identity: "/artists",
    },
    Row {
        name: "audiodb /search.php, artist id",
        provider: Provider::AudioDbArtistSearch,
        body: include_str!("../fixtures/audiodb/search_artist_found.json"),
        kind: PayloadKind::Decode,
        identity: "/artists/0/idArtist",
    },
    Row {
        name: "audiodb /searchalbum.php, album list",
        provider: Provider::AudioDbAlbumSearch,
        body: include_str!("../fixtures/audiodb/search_album_found.json"),
        kind: PayloadKind::Decode,
        identity: "/album",
    },
    Row {
        name: "audiodb /searchalbum.php, album id",
        provider: Provider::AudioDbAlbumSearch,
        body: include_str!("../fixtures/audiodb/search_album_found.json"),
        kind: PayloadKind::Decode,
        identity: "/album/0/idAlbum",
    },
    Row {
        name: "discogs /releases/{id}, release id (7 inch)",
        provider: Provider::DiscogsRelease("249504"),
        body: include_str!("../fixtures/discogs/release_249504.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "discogs /releases/{id}, release id (CD with index rows)",
        provider: Provider::DiscogsRelease("3562468"),
        body: include_str!("../fixtures/discogs/release_3562468.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "discogs /database/search, hit list",
        provider: Provider::DiscogsSearch,
        body: include_str!("../fixtures/discogs/search_found.json"),
        kind: PayloadKind::Decode,
        identity: "/results",
    },
    Row {
        name: "discogs /database/search, hit id",
        provider: Provider::DiscogsSearch,
        body: include_str!("../fixtures/discogs/search_found.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, release id (relations, classical)",
        provider: Provider::MusicBrainzRelease(MB_CLASSICAL_RELEASE_MBID, MB_RELATION_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_relations_classical.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, release id (relations, pop)",
        provider: Provider::MusicBrainzRelease(MB_POP_RELEASE_MBID, MB_RELATION_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_relations_pop.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, artist id (names, Sakamoto)",
        provider: Provider::MusicBrainzRelease(MB_SAKAMOTO_RELEASE_MBID, MB_NAME_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_names_sakamoto.json"),
        kind: PayloadKind::Decode,
        identity: "/artist-credit/0/artist/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, release id (names, Utada)",
        provider: Provider::MusicBrainzRelease(MB_UTADA_RELEASE_MBID, MB_NAME_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_names_utada.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, recording id (names and relations, classical)",
        provider: Provider::MusicBrainzRelease(
            MB_CLASSICAL_RELEASE_MBID,
            MB_NAME_AND_RELATION_INCLUDES,
        ),
        body: include_str!("../fixtures/musicbrainz/release_names_classical.json"),
        kind: PayloadKind::Decode,
        identity: "/media/0/tracks/0/recording/id",
    },
    Row {
        name: "musicbrainz /artist/{mbid}, artist id (merge target)",
        provider: Provider::MusicBrainzArtist(MB_MERGED_ARTIST_MBID, &[]),
        body: include_str!("../fixtures/musicbrainz/artist_merged_canonical.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, release id (all release includes, classical)",
        provider: Provider::MusicBrainzRelease(MB_CLASSICAL_RELEASE_MBID, MB_RELEASE_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_lookup_classical.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, release group id (all release includes, classical)",
        provider: Provider::MusicBrainzRelease(MB_CLASSICAL_RELEASE_MBID, MB_RELEASE_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_lookup_classical.json"),
        kind: PayloadKind::Decode,
        identity: "/release-group/id",
    },
    Row {
        name: "musicbrainz /release/{mbid}, recording id (all release includes, classical)",
        provider: Provider::MusicBrainzRelease(MB_CLASSICAL_RELEASE_MBID, MB_RELEASE_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/release_lookup_classical.json"),
        kind: PayloadKind::Decode,
        identity: "/media/0/tracks/0/recording/id",
    },
    Row {
        name: "musicbrainz /recording/{mbid}, recording id (url relations)",
        provider: Provider::MusicBrainzRecording(MB_RECORDING_MBID, &["url-rels"]),
        body: include_str!("../fixtures/musicbrainz/recording_lookup_url_rels.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /recording/{mbid}, recording id (no include)",
        provider: Provider::MusicBrainzRecording(MB_RECORDING_MBID, &[]),
        body: include_str!("../fixtures/musicbrainz/recording_lookup_plain.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release-group/{mbid}, release group id (all release group includes)",
        provider: Provider::MusicBrainzReleaseGroup(
            MB_OK_COMPUTER_GROUP_MBID,
            MB_RELEASE_GROUP_INCLUDES,
        ),
        body: include_str!("../fixtures/musicbrainz/release_group_lookup_ok_computer.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /release-group/{mbid}, release id (listed releases)",
        provider: Provider::MusicBrainzReleaseGroup(
            MB_OK_COMPUTER_GROUP_MBID,
            MB_RELEASE_GROUP_INCLUDES,
        ),
        body: include_str!("../fixtures/musicbrainz/release_group_lookup_ok_computer.json"),
        kind: PayloadKind::Decode,
        identity: "/releases/0/id",
    },
    Row {
        name: "musicbrainz /artist/{mbid}, artist id (aliases, tags, url relations)",
        provider: Provider::MusicBrainzArtist(MB_RADIOHEAD_MBID, MB_ARTIST_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/artist_lookup_radiohead.json"),
        kind: PayloadKind::Decode,
        identity: "/id",
    },
    Row {
        name: "musicbrainz /url, release id (Discogs release URL, release relations)",
        provider: Provider::MusicBrainzUrl(MB_DISCOGS_RELEASE_URL, MB_URL_INCLUDES),
        body: include_str!("../fixtures/musicbrainz/url_lookup_discogs_release.json"),
        kind: PayloadKind::Decode,
        identity: "/relations/0/release/id",
    },
    Row {
        name: "musicbrainz /artist, search hit id (text)",
        provider: Provider::MusicBrainzArtistSearch("Radiohead"),
        body: include_str!("../fixtures/musicbrainz/search_artist_radiohead.json"),
        kind: PayloadKind::Contract,
        identity: "/artists/0/id",
    },
    Row {
        name: "musicbrainz /release-group, search hit id (text)",
        provider: Provider::MusicBrainzReleaseGroupSearch("OK Computer"),
        body: include_str!("../fixtures/musicbrainz/search_release_group_ok_computer.json"),
        kind: PayloadKind::Contract,
        identity: "/release-groups/0/id",
    },
    Row {
        name: "musicbrainz /release, search hit id (title and artist)",
        provider: Provider::MusicBrainzReleaseSearch("OK Computer", "Radiohead"),
        body: include_str!("../fixtures/musicbrainz/search_release_ok_computer.json"),
        kind: PayloadKind::Contract,
        identity: "/releases/0/id",
    },
    Row {
        name: "musicbrainz /recording, search hit id (title and artist)",
        provider: Provider::MusicBrainzRecordingSearch(
            r#"recording:"Tuesday Morning" AND artist:"Paul Kossoff""#,
        ),
        body: include_str!("../fixtures/musicbrainz/search_recording_tuesday_morning.json"),
        kind: PayloadKind::Contract,
        identity: "/recordings/0/id",
    },
    Row {
        name: "musicbrainz /recording, release id (title and artist)",
        provider: Provider::MusicBrainzRecordingSearch(
            r#"recording:"Tuesday Morning" AND artist:"Paul Kossoff""#,
        ),
        body: include_str!("../fixtures/musicbrainz/search_recording_tuesday_morning.json"),
        kind: PayloadKind::Contract,
        identity: "/recordings/0/releases/0/id",
    },
    Row {
        name: "musicbrainz /release-group?artist, release group id (browse)",
        provider: Provider::MusicBrainzBrowse(MB_RADIOHEAD_MBID),
        body: include_str!("../fixtures/musicbrainz/browse_release_groups_radiohead.json"),
        kind: PayloadKind::Decode,
        identity: "/release-groups/0/id",
    },
    Row {
        name: "lastfm artist.getInfo, artist name",
        provider: Provider::LastFm(LastFmRead::ArtistInfo),
        body: include_str!("../fixtures/lastfm/artist_getinfo.json"),
        kind: PayloadKind::Decode,
        identity: "/artist/name",
    },
    Row {
        name: "lastfm album.getInfo, album title",
        provider: Provider::LastFm(LastFmRead::AlbumInfo),
        body: include_str!("../fixtures/lastfm/album_getinfo.json"),
        kind: PayloadKind::Decode,
        identity: "/album/name",
    },
    Row {
        name: "lastfm artist.getSimilar, artist name",
        provider: Provider::LastFm(LastFmRead::Similar),
        body: include_str!("../fixtures/lastfm/artist_getsimilar.json"),
        kind: PayloadKind::Decode,
        identity: "/similarartists/artist/0/name",
    },
    Row {
        name: "lastfm artist.getSimilar, list",
        provider: Provider::LastFm(LastFmRead::Similar),
        body: include_str!("../fixtures/lastfm/artist_getsimilar.json"),
        kind: PayloadKind::Decode,
        identity: "/similarartists",
    },
    Row {
        name: "lastfm artist.getTopTracks, track name",
        provider: Provider::LastFm(LastFmRead::ArtistTopTracks),
        body: include_str!("../fixtures/lastfm/artist_gettoptracks.json"),
        kind: PayloadKind::Decode,
        identity: "/toptracks/track/0/name",
    },
    Row {
        name: "lastfm artist.getTopAlbums, album name",
        provider: Provider::LastFm(LastFmRead::ArtistTopAlbums),
        body: include_str!("../fixtures/lastfm/artist_gettopalbums.json"),
        kind: PayloadKind::Decode,
        identity: "/topalbums/album/0/name",
    },
    Row {
        name: "lastfm artist.getTopTags, tag name",
        provider: Provider::LastFm(LastFmRead::ArtistTopTags),
        body: include_str!("../fixtures/lastfm/artist_gettoptags.json"),
        kind: PayloadKind::Decode,
        identity: "/toptags/tag/0/name",
    },
    Row {
        name: "lastfm album.getTopTags, tag name",
        provider: Provider::LastFm(LastFmRead::AlbumTopTags),
        body: include_str!("../fixtures/lastfm/album_gettoptags.json"),
        kind: PayloadKind::Decode,
        identity: "/toptags/tag/0/name",
    },
    Row {
        name: "lastfm chart.getTopArtists, artist name",
        provider: Provider::LastFm(LastFmRead::ChartTopArtists),
        body: include_str!("../fixtures/lastfm/chart_gettopartists.json"),
        kind: PayloadKind::Decode,
        identity: "/artists/artist/0/name",
    },
    Row {
        name: "lastfm tag.getTopArtists, artist name",
        provider: Provider::LastFm(LastFmRead::TagTopArtists),
        body: include_str!("../fixtures/lastfm/tag_gettopartists.json"),
        kind: PayloadKind::Decode,
        identity: "/topartists/artist/0/name",
    },
    Row {
        name: "lastfm tag.getTopArtists, list",
        provider: Provider::LastFm(LastFmRead::TagTopArtists),
        body: include_str!("../fixtures/lastfm/tag_gettopartists.json"),
        kind: PayloadKind::Decode,
        identity: "/topartists",
    },
    Row {
        name: "skiddle /artists/, act id",
        provider: Provider::SkiddleArtists,
        body: include_str!("../fixtures/skiddle/artists_search.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/id",
    },
    Row {
        name: "skiddle /artists/, act name",
        provider: Provider::SkiddleArtists,
        body: include_str!("../fixtures/skiddle/artists_search.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/name",
    },
    Row {
        name: "skiddle /events/search/, event id",
        provider: Provider::SkiddleEvents,
        body: include_str!("../fixtures/skiddle/events_for_artist.json"),
        kind: PayloadKind::Decode,
        identity: "/results/0/id",
    },
    Row {
        name: "ticketmaster /attractions.json, act id",
        provider: Provider::TicketmasterAttractions,
        body: include_str!("../fixtures/ticketmaster/attractions_search_radiohead.json"),
        kind: PayloadKind::Decode,
        identity: "/_embedded/attractions/0/id",
    },
    Row {
        name: "ticketmaster /attractions.json, act name",
        provider: Provider::TicketmasterAttractions,
        body: include_str!("../fixtures/ticketmaster/attractions_search_radiohead.json"),
        kind: PayloadKind::Decode,
        identity: "/_embedded/attractions/0/name",
    },
    Row {
        name: "ticketmaster /events.json, event id",
        provider: Provider::TicketmasterEvents,
        body: include_str!("../fixtures/ticketmaster/events_fontaines_dc.json"),
        kind: PayloadKind::Decode,
        identity: "/_embedded/events/0/id",
    },
    Row {
        name: "ticketmaster /events.json, attraction id",
        provider: Provider::TicketmasterEvents,
        body: include_str!("../fixtures/ticketmaster/events_fontaines_dc.json"),
        kind: PayloadKind::Decode,
        identity: "/_embedded/events/0/_embedded/attractions/0/id",
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
        Provider::TicketmasterAttractions => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/ticketmaster/attractions.json",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    ticketmaster: "http://fake/ticketmaster".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let found = providers
                .ticketmaster
                .search_attractions("k", "Radiohead", PRIORITY)
                .await?;
            Ok(!found.is_empty())
        }
        Provider::TicketmasterEvents => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/ticketmaster/events.json",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    ticketmaster: "http://fake/ticketmaster".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let page = providers
                .ticketmaster
                .events_page("k", "K8vZ9179LP7", 0, PRIORITY)
                .await?;
            Ok(!page.events.is_empty())
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
        Provider::SkiddleArtists => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/skiddle/artists/",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    skiddle: "http://fake/skiddle".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let acts = providers
                .skiddle
                .search_artists("k", "Orbital", PRIORITY)
                .await?;
            Ok(!acts.is_empty())
        }
        Provider::SkiddleEvents => {
            let wire = Arc::new(ScriptedWire::new());
            wire.answer(
                "http://fake/skiddle/events/search/",
                200,
                &[],
                body.as_bytes().to_vec(),
            );
            let providers = Providers::unpaced_at(
                wire,
                ProviderEndpoints {
                    skiddle: "http://fake/skiddle".to_owned(),
                    ..ProviderEndpoints::closed()
                },
            );
            let events = providers
                .skiddle
                .events_for_artist("k", "123487626", PRIORITY)
                .await?;
            Ok(!events.is_empty())
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
        Provider::MusicBrainzRelease(mbid, includes) => {
            let providers = musicbrainz_answer(&format!("/release/{mbid}"), body);
            let release = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .lookup_release(mbid, includes, Criticality::IdentityCritical)
                .await?;
            Ok(release.is_some())
        }
        Provider::MusicBrainzReleaseGroup(mbid, includes) => {
            let providers = musicbrainz_answer(&format!("/release-group/{mbid}"), body);
            let group = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .lookup_release_group(mbid, includes, Criticality::IdentityCritical)
                .await?;
            Ok(group.is_some())
        }
        Provider::MusicBrainzArtist(mbid, includes) => {
            let providers = musicbrainz_answer(&format!("/artist/{mbid}"), body);
            let artist = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .lookup_artist(mbid, includes, Criticality::IdentityCritical)
                .await?;
            Ok(artist.is_some())
        }
        Provider::MusicBrainzRecording(mbid, includes) => {
            let providers = musicbrainz_answer(&format!("/recording/{mbid}"), body);
            let recording = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .lookup_recording(mbid, includes, Criticality::IdentityCritical)
                .await?;
            Ok(recording.is_some())
        }
        Provider::MusicBrainzUrl(resource, includes) => {
            let providers = musicbrainz_answer("/url", body);
            let resolution = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .resolve_url(resource, includes, Criticality::IdentityCritical)
                .await?;
            Ok(!resolution.relations.is_empty())
        }
        Provider::MusicBrainzArtistSearch(text) => {
            let providers = musicbrainz_answer("/artist", body);
            let page = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .search_artists_text(text, 5, 0, Criticality::IdentityCritical)
                .await?;
            Ok(!page.items.is_empty())
        }
        Provider::MusicBrainzReleaseGroupSearch(text) => {
            let providers = musicbrainz_answer("/release-group", body);
            let page = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .search_release_groups_text(text, 5, 0, Criticality::IdentityCritical)
                .await?;
            Ok(!page.items.is_empty())
        }
        Provider::MusicBrainzReleaseSearch(title, artist) => {
            let providers = musicbrainz_answer("/release", body);
            let page = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .search_releases(title, artist, 5, Criticality::IdentityCritical)
                .await?;
            Ok(!page.items.is_empty())
        }
        Provider::MusicBrainzRecordingSearch(query) => {
            let providers = musicbrainz_answer("/recording", body);
            let page = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .search_recordings_query(query, 3, Criticality::IdentityCritical)
                .await?;
            Ok(page
                .items
                .first()
                .is_some_and(|hit| !hit.releases.is_empty()))
        }
        Provider::MusicBrainzBrowse(artist) => {
            let providers = musicbrainz_answer("/release-group", body);
            let page = providers
                .musicbrainz
                .fixed(MbSource::official(), PRIORITY)
                .browse_artist_release_groups(artist, 5, 0, Criticality::IdentityCritical)
                .await?;
            Ok(!page.items.is_empty())
        }
        Provider::LastFm(read) => lastfm(read, body).await,
    }
}

/// Serves `body` from Last.fm's API root and reads it through `read`.
async fn lastfm(read: LastFmRead, body: &str) -> Result<bool, ProviderError> {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer("http://fake/lastfm/", 200, &[], body.as_bytes().to_vec());
    let providers = Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            lastfm: "http://fake/lastfm/".to_owned(),
            ..ProviderEndpoints::closed()
        },
    );
    let client = &providers.lastfm;
    let creds = LastFmCredentials {
        api_key: "key".to_owned(),
        ..LastFmCredentials::default()
    };
    Ok(match read {
        LastFmRead::ArtistInfo => client
            .artist_info(&creds, "Radiohead", None, PRIORITY)
            .await?
            .is_some_and(|artist| !artist.similar.is_empty() && !artist.tags.is_empty()),
        LastFmRead::AlbumInfo => client
            .album_info(&creds, "Radiohead", "OK Computer", None, PRIORITY)
            .await?
            .is_some_and(|album| album.tracks.len() == 12),
        LastFmRead::Similar => !client
            .similar_artists(&creds, "Radiohead", None, 3, PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::ArtistTopTracks => !client
            .artist_top_tracks(&creds, "Radiohead", None, 3, PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::ArtistTopAlbums => !client
            .artist_top_albums(&creds, "Radiohead", None, 3, PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::ArtistTopTags => !client
            .artist_top_genres(&creds, "Radiohead", PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::AlbumTopTags => !client
            .album_top_genres(&creds, "Radiohead", "OK Computer", PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::ChartTopArtists => !client
            .chart_top_artists(&creds, 3, PRIORITY)
            .await?
            .is_empty(),
        LastFmRead::TagTopArtists => !client
            .tag_top_artists(&creds, "trip-hop", 1, PRIORITY)
            .await?
            .is_empty(),
    })
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

/// Serves `body` from the MusicBrainz address for `path` and returns the
/// providers that read it.
fn musicbrainz_answer(path: &str, body: &str) -> Providers {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}{path}", musicbrainz::MB_API_BASE),
        200,
        &[],
        body.as_bytes().to_vec(),
    );
    Providers::unpaced_at(
        wire,
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
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
            matches!(&error, ProviderError::Payload { kind, .. } if *kind == row.kind),
            "{}: without `{}` the answer must fail as {:?}: {error:?}",
            row.name,
            row.identity,
            row.kind
        );
    }
}
