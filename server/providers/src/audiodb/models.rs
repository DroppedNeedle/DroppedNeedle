//! TheAudioDB wire shapes, read by the lookups in `audiodb.rs`.
//!
//! Tolerant of unknown fields. An entry without its id fails decoding, and so
//! does an answer without its list: a missing `artists` or `album` key is a
//! changed answer, never a lookup with no match. A list that is `null` or
//! empty is the no-match answer. The artwork and the other fields are optional.

use serde::{Deserialize, Deserializer, Serialize};

/// Reads a list the answer must carry. `null` reads as no entries.
///
/// The field is a `Vec`, not an `Option`, because serde reads a missing
/// `Option` field as `None`. A missing `Vec` field fails decoding.
fn present_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// The answer to `artist-mb.php` and `search.php`.
///
/// Verified against www.theaudiodb.com/api/v1/json/123/artist-mb.php on 2026-10-21: tests/fixtures/audiodb/artist_mb_found.json.
/// Verified against www.theaudiodb.com/api/v1/json/123/search.php on 2026-10-21: tests/fixtures/audiodb/search_artist_found.json.
/// Nothing found, recorded on 2026-10-21: tests/fixtures/audiodb/artist_mb_not_found.json.
#[derive(Debug, Deserialize)]
pub struct ArtistAnswer {
    /// The matching artists, best first. Required, though it may be `null`.
    #[serde(rename = "artists", deserialize_with = "present_list")]
    pub artists: Vec<AudioDbArtist>,
}

/// The answer to `album-mb.php` and `searchalbum.php`.
///
/// Verified against www.theaudiodb.com/api/v1/json/123/album-mb.php on 2026-10-21: tests/fixtures/audiodb/album_mb_found.json.
/// Verified against www.theaudiodb.com/api/v1/json/123/searchalbum.php on 2026-10-21: tests/fixtures/audiodb/search_album_found.json.
#[derive(Debug, Deserialize)]
pub struct AlbumAnswer {
    /// The matching albums, best first. Required, though it may be `null`.
    #[serde(rename = "album", deserialize_with = "present_list")]
    pub album: Vec<AudioDbAlbum>,
}

/// Artist artwork. The id and the name are required, so an entry without
/// them fails decoding instead of yielding an empty success. Every artwork
/// URL stays optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDbArtist {
    /// TheAudioDB artist id.
    #[serde(rename = "idArtist")]
    pub id_artist: String,
    /// Artist name.
    #[serde(rename = "strArtist")]
    pub name: String,
    /// MusicBrainz artist id, when the entry carries one.
    #[serde(rename = "strMusicBrainzID", default)]
    pub mbid: Option<String>,
    /// Artist thumbnail URL.
    #[serde(rename = "strArtistThumb", default)]
    pub thumb: Option<String>,
    /// Artist fanart URL.
    #[serde(rename = "strArtistFanart", default)]
    pub fanart: Option<String>,
    /// Second fanart URL.
    #[serde(rename = "strArtistFanart2", default)]
    pub fanart_2: Option<String>,
    /// Third fanart URL.
    #[serde(rename = "strArtistFanart3", default)]
    pub fanart_3: Option<String>,
    /// Fourth fanart URL.
    #[serde(rename = "strArtistFanart4", default)]
    pub fanart_4: Option<String>,
    /// Wide thumbnail URL.
    #[serde(rename = "strArtistWideThumb", default)]
    pub wide_thumb: Option<String>,
    /// Banner URL.
    #[serde(rename = "strArtistBanner", default)]
    pub banner: Option<String>,
    /// Logo URL.
    #[serde(rename = "strArtistLogo", default)]
    pub logo: Option<String>,
    /// Cutout URL.
    #[serde(rename = "strArtistCutout", default)]
    pub cutout: Option<String>,
    /// Clearart URL.
    #[serde(rename = "strArtistClearart", default)]
    pub clearart: Option<String>,
}

/// Album artwork. The id and the title are required, as for the artist.
/// Artwork stays optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDbAlbum {
    /// TheAudioDB album id.
    #[serde(rename = "idAlbum")]
    pub id_album: String,
    /// Album title.
    #[serde(rename = "strAlbum")]
    pub title: String,
    /// MusicBrainz release id, when the entry carries one.
    #[serde(rename = "strMusicBrainzID", default)]
    pub mbid: Option<String>,
    /// Album thumbnail URL.
    #[serde(rename = "strAlbumThumb", default)]
    pub thumb: Option<String>,
    /// Back cover URL.
    #[serde(rename = "strAlbumBack", default)]
    pub back: Option<String>,
    /// CD art URL.
    #[serde(rename = "strAlbumCDart", default)]
    pub cdart: Option<String>,
    /// Spine URL.
    #[serde(rename = "strAlbumSpine", default)]
    pub spine: Option<String>,
    /// 3D case URL.
    #[serde(rename = "strAlbum3DCase", default)]
    pub case_3d: Option<String>,
    /// 3D flat URL.
    #[serde(rename = "strAlbum3DFlat", default)]
    pub flat_3d: Option<String>,
    /// 3D face URL.
    #[serde(rename = "strAlbum3DFace", default)]
    pub face_3d: Option<String>,
    /// 3D thumbnail URL.
    #[serde(rename = "strAlbum3DThumb", default)]
    pub thumb_3d: Option<String>,
}
