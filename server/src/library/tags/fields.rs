//! The fields the save wrapper writes: Picard's tag set, with Picard's
//! names per format (<https://picard-docs.musicbrainz.org/en/appendices/tag_mapping.html>),
//! so beets, Lidarr, Navidrome, Jellyfin, and Picard itself read them.

/// One writable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TagField {
    Title,
    Artist,
    Artists,
    Album,
    AlbumArtist,
    AlbumArtistSort,
    ArtistSort,
    Genre,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    Date,
    OriginalDate,
    ReleaseStatus,
    ReleaseCountry,
    ReleaseType,
    Media,
    Label,
    CatalogNumber,
    Barcode,
    Asin,
    MusicBrainzRecordingId,
    MusicBrainzReleaseTrackId,
    MusicBrainzReleaseId,
    MusicBrainzReleaseGroupId,
    MusicBrainzArtistId,
    MusicBrainzAlbumArtistId,
}

/// How a field's values must look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Free text, one or more values.
    Text,
    /// One non-negative integer.
    Count,
    /// One `YYYY`, `YYYY-MM`, or `YYYY-MM-DD` date.
    Date,
    /// One identifier.
    Single,
}

/// Which half of an `n/m` pair (ID3 `TRCK`/`TPOS`, MP4 `trkn`/`disk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Number,
    Total,
}

/// Where a field lives in an ID3v2 tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id3Target {
    Text(&'static str),
    /// `TXXX` with this description.
    User(&'static str),
    /// `UFID` with this owner.
    Ufid(&'static str),
    Pair(&'static str, Slot),
}

/// Where a field lives in an MP4 `ilst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Target {
    Text([u8; 4]),
    /// `----:com.apple.iTunes:<name>`.
    Freeform(&'static str),
    Pair([u8; 4], Slot),
}

/// The freeform namespace Picard and iTunes use.
pub const MP4_MEAN: &str = "com.apple.iTunes";
/// The UFID owner MusicBrainz recording ids live under.
pub const MUSICBRAINZ_UFID_OWNER: &str = "http://musicbrainz.org";

impl TagField {
    pub const ALL: [TagField; 28] = [
        TagField::Title,
        TagField::Artist,
        TagField::Artists,
        TagField::Album,
        TagField::AlbumArtist,
        TagField::AlbumArtistSort,
        TagField::ArtistSort,
        TagField::Genre,
        TagField::TrackNumber,
        TagField::TrackTotal,
        TagField::DiscNumber,
        TagField::DiscTotal,
        TagField::Date,
        TagField::OriginalDate,
        TagField::ReleaseStatus,
        TagField::ReleaseCountry,
        TagField::ReleaseType,
        TagField::Media,
        TagField::Label,
        TagField::CatalogNumber,
        TagField::Barcode,
        TagField::Asin,
        TagField::MusicBrainzRecordingId,
        TagField::MusicBrainzReleaseTrackId,
        TagField::MusicBrainzReleaseId,
        TagField::MusicBrainzReleaseGroupId,
        TagField::MusicBrainzArtistId,
        TagField::MusicBrainzAlbumArtistId,
    ];

    /// The managed-field name publish requests and documents use.
    pub fn name(self) -> &'static str {
        match self {
            TagField::Title => "title",
            TagField::Artist => "artist",
            TagField::Artists => "artists",
            TagField::Album => "album",
            TagField::AlbumArtist => "album_artist",
            TagField::AlbumArtistSort => "album_artist_sort",
            TagField::ArtistSort => "artist_sort",
            TagField::Genre => "genre",
            TagField::TrackNumber => "track_number",
            TagField::TrackTotal => "total_tracks",
            TagField::DiscNumber => "disc_number",
            TagField::DiscTotal => "total_discs",
            TagField::Date => "date",
            TagField::OriginalDate => "original_date",
            TagField::ReleaseStatus => "release_status",
            TagField::ReleaseCountry => "release_country",
            TagField::ReleaseType => "release_type",
            TagField::Media => "media",
            TagField::Label => "label",
            TagField::CatalogNumber => "catalog_number",
            TagField::Barcode => "barcode",
            TagField::Asin => "asin",
            TagField::MusicBrainzRecordingId => "musicbrainz_recording_id",
            TagField::MusicBrainzReleaseTrackId => "musicbrainz_release_track_id",
            TagField::MusicBrainzReleaseId => "musicbrainz_release_id",
            TagField::MusicBrainzReleaseGroupId => "musicbrainz_release_group_id",
            TagField::MusicBrainzArtistId => "musicbrainz_artist_id",
            TagField::MusicBrainzAlbumArtistId => "musicbrainz_album_artist_id",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        TagField::ALL.into_iter().find(|field| field.name() == name)
    }

    pub fn kind(self) -> FieldKind {
        match self {
            TagField::TrackNumber
            | TagField::TrackTotal
            | TagField::DiscNumber
            | TagField::DiscTotal => FieldKind::Count,
            TagField::Date | TagField::OriginalDate => FieldKind::Date,
            TagField::ReleaseStatus
            | TagField::ReleaseCountry
            | TagField::Barcode
            | TagField::Asin
            | TagField::Media
            | TagField::MusicBrainzRecordingId
            | TagField::MusicBrainzReleaseTrackId
            | TagField::MusicBrainzReleaseId
            | TagField::MusicBrainzReleaseGroupId => FieldKind::Single,
            _ => FieldKind::Text,
        }
    }

    /// The Vorbis comment key. Totals may also be spelled `TRACKTOTAL` /
    /// `DISCTOTAL`; see [`TagField::vorbis_aliases`].
    pub fn vorbis_key(self) -> &'static str {
        match self {
            TagField::Title => "TITLE",
            TagField::Artist => "ARTIST",
            TagField::Artists => "ARTISTS",
            TagField::Album => "ALBUM",
            TagField::AlbumArtist => "ALBUMARTIST",
            TagField::AlbumArtistSort => "ALBUMARTISTSORT",
            TagField::ArtistSort => "ARTISTSORT",
            TagField::Genre => "GENRE",
            TagField::TrackNumber => "TRACKNUMBER",
            TagField::TrackTotal => "TOTALTRACKS",
            TagField::DiscNumber => "DISCNUMBER",
            TagField::DiscTotal => "TOTALDISCS",
            TagField::Date => "DATE",
            TagField::OriginalDate => "ORIGINALDATE",
            TagField::ReleaseStatus => "RELEASESTATUS",
            TagField::ReleaseCountry => "RELEASECOUNTRY",
            TagField::ReleaseType => "RELEASETYPE",
            TagField::Media => "MEDIA",
            TagField::Label => "LABEL",
            TagField::CatalogNumber => "CATALOGNUMBER",
            TagField::Barcode => "BARCODE",
            TagField::Asin => "ASIN",
            TagField::MusicBrainzRecordingId => "MUSICBRAINZ_TRACKID",
            TagField::MusicBrainzReleaseTrackId => "MUSICBRAINZ_RELEASETRACKID",
            TagField::MusicBrainzReleaseId => "MUSICBRAINZ_ALBUMID",
            TagField::MusicBrainzReleaseGroupId => "MUSICBRAINZ_RELEASEGROUPID",
            TagField::MusicBrainzArtistId => "MUSICBRAINZ_ARTISTID",
            TagField::MusicBrainzAlbumArtistId => "MUSICBRAINZ_ALBUMARTISTID",
        }
    }

    /// Other spellings readers accept for the same Vorbis field.
    pub fn vorbis_aliases(self) -> &'static [&'static str] {
        match self {
            TagField::TrackTotal => &["TRACKTOTAL"],
            TagField::DiscTotal => &["DISCTOTAL"],
            _ => &[],
        }
    }

    /// The ID3v2 home. Version 2.3 has no `TDRC`, `TDOR`, `TSOP`, or
    /// `TSO2`: dates go to `TYER`/`TORY` (year only) and sort names to
    /// `TXXX`, so the tag stays a clean v2.3 tag.
    pub fn id3(self, v23: bool) -> Id3Target {
        match self {
            TagField::Title => Id3Target::Text("TIT2"),
            TagField::Artist => Id3Target::Text("TPE1"),
            TagField::Artists => Id3Target::User("ARTISTS"),
            TagField::Album => Id3Target::Text("TALB"),
            TagField::AlbumArtist => Id3Target::Text("TPE2"),
            TagField::AlbumArtistSort if v23 => Id3Target::User("ALBUMARTISTSORT"),
            TagField::AlbumArtistSort => Id3Target::Text("TSO2"),
            TagField::ArtistSort if v23 => Id3Target::User("ARTISTSORT"),
            TagField::ArtistSort => Id3Target::Text("TSOP"),
            TagField::Genre => Id3Target::Text("TCON"),
            TagField::TrackNumber => Id3Target::Pair("TRCK", Slot::Number),
            TagField::TrackTotal => Id3Target::Pair("TRCK", Slot::Total),
            TagField::DiscNumber => Id3Target::Pair("TPOS", Slot::Number),
            TagField::DiscTotal => Id3Target::Pair("TPOS", Slot::Total),
            TagField::Date if v23 => Id3Target::Text("TYER"),
            TagField::Date => Id3Target::Text("TDRC"),
            TagField::OriginalDate if v23 => Id3Target::Text("TORY"),
            TagField::OriginalDate => Id3Target::Text("TDOR"),
            TagField::ReleaseStatus => Id3Target::User("MusicBrainz Album Status"),
            TagField::ReleaseCountry => Id3Target::User("MusicBrainz Album Release Country"),
            TagField::ReleaseType => Id3Target::User("MusicBrainz Album Type"),
            TagField::Media => Id3Target::Text("TMED"),
            TagField::Label => Id3Target::Text("TPUB"),
            TagField::CatalogNumber => Id3Target::User("CATALOGNUMBER"),
            TagField::Barcode => Id3Target::User("BARCODE"),
            TagField::Asin => Id3Target::User("ASIN"),
            TagField::MusicBrainzRecordingId => Id3Target::Ufid(MUSICBRAINZ_UFID_OWNER),
            TagField::MusicBrainzReleaseTrackId => Id3Target::User("MusicBrainz Release Track Id"),
            TagField::MusicBrainzReleaseId => Id3Target::User("MusicBrainz Album Id"),
            TagField::MusicBrainzReleaseGroupId => Id3Target::User("MusicBrainz Release Group Id"),
            TagField::MusicBrainzArtistId => Id3Target::User("MusicBrainz Artist Id"),
            TagField::MusicBrainzAlbumArtistId => Id3Target::User("MusicBrainz Album Artist Id"),
        }
    }

    pub fn mp4(self) -> Mp4Target {
        match self {
            TagField::Title => Mp4Target::Text(*b"\xa9nam"),
            TagField::Artist => Mp4Target::Text(*b"\xa9ART"),
            TagField::Artists => Mp4Target::Freeform("ARTISTS"),
            TagField::Album => Mp4Target::Text(*b"\xa9alb"),
            TagField::AlbumArtist => Mp4Target::Text(*b"aART"),
            TagField::AlbumArtistSort => Mp4Target::Text(*b"soaa"),
            TagField::ArtistSort => Mp4Target::Text(*b"soar"),
            TagField::Genre => Mp4Target::Text(*b"\xa9gen"),
            TagField::TrackNumber => Mp4Target::Pair(*b"trkn", Slot::Number),
            TagField::TrackTotal => Mp4Target::Pair(*b"trkn", Slot::Total),
            TagField::DiscNumber => Mp4Target::Pair(*b"disk", Slot::Number),
            TagField::DiscTotal => Mp4Target::Pair(*b"disk", Slot::Total),
            TagField::Date => Mp4Target::Text(*b"\xa9day"),
            TagField::OriginalDate => Mp4Target::Freeform("ORIGINALDATE"),
            TagField::ReleaseStatus => Mp4Target::Freeform("MusicBrainz Album Status"),
            TagField::ReleaseCountry => Mp4Target::Freeform("MusicBrainz Album Release Country"),
            TagField::ReleaseType => Mp4Target::Freeform("MusicBrainz Album Type"),
            TagField::Media => Mp4Target::Freeform("MEDIA"),
            TagField::Label => Mp4Target::Freeform("LABEL"),
            TagField::CatalogNumber => Mp4Target::Freeform("CATALOGNUMBER"),
            TagField::Barcode => Mp4Target::Freeform("BARCODE"),
            TagField::Asin => Mp4Target::Freeform("ASIN"),
            TagField::MusicBrainzRecordingId => Mp4Target::Freeform("MusicBrainz Track Id"),
            TagField::MusicBrainzReleaseTrackId => {
                Mp4Target::Freeform("MusicBrainz Release Track Id")
            }
            TagField::MusicBrainzReleaseId => Mp4Target::Freeform("MusicBrainz Album Id"),
            TagField::MusicBrainzReleaseGroupId => {
                Mp4Target::Freeform("MusicBrainz Release Group Id")
            }
            TagField::MusicBrainzArtistId => Mp4Target::Freeform("MusicBrainz Artist Id"),
            TagField::MusicBrainzAlbumArtistId => {
                Mp4Target::Freeform("MusicBrainz Album Artist Id")
            }
        }
    }
}
