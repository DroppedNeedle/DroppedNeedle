//! The fields the save wrapper writes: Picard's tag set, with Picard's
//! names per format (<https://picard-docs.musicbrainz.org/en/appendices/tag_mapping.html>),
//! so beets, Lidarr, Navidrome, Jellyfin, and Picard itself read them.

/// One writable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TagField {
    Title,
    TitleSort,
    Artist,
    Artists,
    ArtistSort,
    Album,
    AlbumSort,
    AlbumArtist,
    AlbumArtistSort,
    Genre,
    Compilation,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    DiscSubtitle,
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

/// How a new value for a field must look. Values replayed from a file
/// (undo, baseline restore) are written back as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Free text, one or more values.
    Text,
    /// One non-negative integer.
    Count,
    /// One `YYYY`, `YYYY-MM`, or `YYYY-MM-DD` date.
    Date,
    /// One identifier or word.
    Single,
    /// `1` or `0`.
    Flag,
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
    /// ID3v2.3's split date: `TYER` (year), `TDAT` (`DDMM`), `TIME`
    /// (`HHMM`).
    SplitDate,
}

/// Where a field lives in an MP4 `ilst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Target {
    Text([u8; 4]),
    /// `----:com.apple.iTunes:<name>`.
    Freeform(&'static str),
    Pair([u8; 4], Slot),
    /// A one-byte boolean atom.
    Flag([u8; 4]),
}

/// The freeform namespace Picard and iTunes use.
pub const MP4_MEAN: &str = "com.apple.iTunes";
/// The UFID owner MusicBrainz recording ids live under.
pub const MUSICBRAINZ_UFID_OWNER: &str = "http://musicbrainz.org";

impl TagField {
    pub const ALL: [TagField; 32] = [
        TagField::Title,
        TagField::TitleSort,
        TagField::Artist,
        TagField::Artists,
        TagField::ArtistSort,
        TagField::Album,
        TagField::AlbumSort,
        TagField::AlbumArtist,
        TagField::AlbumArtistSort,
        TagField::Genre,
        TagField::Compilation,
        TagField::TrackNumber,
        TagField::TrackTotal,
        TagField::DiscNumber,
        TagField::DiscTotal,
        TagField::DiscSubtitle,
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
            TagField::TitleSort => "title_sort",
            TagField::Artist => "artist",
            TagField::Artists => "artists",
            TagField::ArtistSort => "artist_sort",
            TagField::Album => "album",
            TagField::AlbumSort => "album_sort",
            TagField::AlbumArtist => "album_artist",
            TagField::AlbumArtistSort => "album_artist_sort",
            TagField::Genre => "genre",
            TagField::Compilation => "compilation",
            TagField::TrackNumber => "track_number",
            TagField::TrackTotal => "total_tracks",
            TagField::DiscNumber => "disc_number",
            TagField::DiscTotal => "total_discs",
            TagField::DiscSubtitle => "disc_subtitle",
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

    /// The managed name for one Vorbis spelling of a field
    /// (`total_tracks:TRACKTOTAL`). Only documents read from a file carry
    /// these; requests cannot.
    pub fn spelling_name(self, spelling: &str) -> String {
        format!("{}:{spelling}", self.name())
    }

    /// Parse a managed name: a field, or a field with one of its Vorbis
    /// spellings.
    pub fn from_managed_name(name: &str) -> Option<(Self, Option<&'static str>)> {
        match name.split_once(':') {
            None => Self::from_name(name).map(|field| (field, None)),
            Some((field, spelling)) => {
                let field = Self::from_name(field)?;
                let spelling = field
                    .vorbis_keys()
                    .iter()
                    .copied()
                    .find(|key| *key == spelling)?;
                Some((field, Some(spelling)))
            }
        }
    }

    pub fn kind(self) -> FieldKind {
        match self {
            TagField::TrackNumber
            | TagField::TrackTotal
            | TagField::DiscNumber
            | TagField::DiscTotal => FieldKind::Count,
            TagField::Date | TagField::OriginalDate => FieldKind::Date,
            TagField::Compilation => FieldKind::Flag,
            TagField::ReleaseStatus
            | TagField::ReleaseCountry
            | TagField::Barcode
            | TagField::Asin
            | TagField::Media
            | TagField::DiscSubtitle
            | TagField::MusicBrainzRecordingId
            | TagField::MusicBrainzReleaseTrackId
            | TagField::MusicBrainzReleaseId
            | TagField::MusicBrainzReleaseGroupId => FieldKind::Single,
            _ => FieldKind::Text,
        }
    }

    /// Every Vorbis comment key the field goes by, Picard's first. A
    /// write updates each spelling the file already uses (Picard's own
    /// first when it uses none), so readers of either agree.
    pub fn vorbis_keys(self) -> &'static [&'static str] {
        match self {
            TagField::Title => &["TITLE"],
            TagField::TitleSort => &["TITLESORT"],
            TagField::Artist => &["ARTIST"],
            TagField::Artists => &["ARTISTS"],
            TagField::ArtistSort => &["ARTISTSORT"],
            TagField::Album => &["ALBUM"],
            TagField::AlbumSort => &["ALBUMSORT"],
            TagField::AlbumArtist => &["ALBUMARTIST"],
            TagField::AlbumArtistSort => &["ALBUMARTISTSORT"],
            TagField::Genre => &["GENRE"],
            TagField::Compilation => &["COMPILATION"],
            TagField::TrackNumber => &["TRACKNUMBER"],
            TagField::TrackTotal => &["TOTALTRACKS", "TRACKTOTAL"],
            TagField::DiscNumber => &["DISCNUMBER"],
            TagField::DiscTotal => &["TOTALDISCS", "DISCTOTAL"],
            TagField::DiscSubtitle => &["DISCSUBTITLE"],
            TagField::Date => &["DATE"],
            TagField::OriginalDate => &["ORIGINALDATE"],
            TagField::ReleaseStatus => &["RELEASESTATUS"],
            TagField::ReleaseCountry => &["RELEASECOUNTRY"],
            TagField::ReleaseType => &["RELEASETYPE"],
            TagField::Media => &["MEDIA"],
            TagField::Label => &["LABEL"],
            TagField::CatalogNumber => &["CATALOGNUMBER"],
            TagField::Barcode => &["BARCODE"],
            TagField::Asin => &["ASIN"],
            TagField::MusicBrainzRecordingId => &["MUSICBRAINZ_TRACKID"],
            TagField::MusicBrainzReleaseTrackId => &["MUSICBRAINZ_RELEASETRACKID"],
            TagField::MusicBrainzReleaseId => &["MUSICBRAINZ_ALBUMID"],
            TagField::MusicBrainzReleaseGroupId => &["MUSICBRAINZ_RELEASEGROUPID"],
            TagField::MusicBrainzArtistId => &["MUSICBRAINZ_ARTISTID"],
            TagField::MusicBrainzAlbumArtistId => &["MUSICBRAINZ_ALBUMARTISTID"],
        }
    }

    /// The ID3v2 home. In version 2.3 the date goes to
    /// `TYER`/`TDAT`/`TIME` and the original date to `TORY` (year only).
    /// lofty's v2.3 writer drops `TSOP`, `TSOA`, `TSOT`, and `TSST`, so in
    /// v2.3 those fields go to `TXXX` frames; `TSO2` survives and is used in
    /// both versions.
    pub fn id3(self, v23: bool) -> Id3Target {
        match self {
            TagField::Title => Id3Target::Text("TIT2"),
            TagField::TitleSort if v23 => Id3Target::User("TITLESORT"),
            TagField::TitleSort => Id3Target::Text("TSOT"),
            TagField::Artist => Id3Target::Text("TPE1"),
            TagField::Artists => Id3Target::User("ARTISTS"),
            TagField::ArtistSort if v23 => Id3Target::User("ARTISTSORT"),
            TagField::ArtistSort => Id3Target::Text("TSOP"),
            TagField::Album => Id3Target::Text("TALB"),
            TagField::AlbumSort if v23 => Id3Target::User("ALBUMSORT"),
            TagField::AlbumSort => Id3Target::Text("TSOA"),
            TagField::AlbumArtist => Id3Target::Text("TPE2"),
            TagField::AlbumArtistSort => Id3Target::Text("TSO2"),
            TagField::Genre => Id3Target::Text("TCON"),
            TagField::Compilation => Id3Target::Text("TCMP"),
            TagField::TrackNumber => Id3Target::Pair("TRCK", Slot::Number),
            TagField::TrackTotal => Id3Target::Pair("TRCK", Slot::Total),
            TagField::DiscNumber => Id3Target::Pair("TPOS", Slot::Number),
            TagField::DiscTotal => Id3Target::Pair("TPOS", Slot::Total),
            TagField::DiscSubtitle if v23 => Id3Target::User("DISCSUBTITLE"),
            TagField::DiscSubtitle => Id3Target::Text("TSST"),
            TagField::Date if v23 => Id3Target::SplitDate,
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
            TagField::TitleSort => Mp4Target::Text(*b"sonm"),
            TagField::Artist => Mp4Target::Text(*b"\xa9ART"),
            TagField::Artists => Mp4Target::Freeform("ARTISTS"),
            TagField::ArtistSort => Mp4Target::Text(*b"soar"),
            TagField::Album => Mp4Target::Text(*b"\xa9alb"),
            TagField::AlbumSort => Mp4Target::Text(*b"soal"),
            TagField::AlbumArtist => Mp4Target::Text(*b"aART"),
            TagField::AlbumArtistSort => Mp4Target::Text(*b"soaa"),
            TagField::Genre => Mp4Target::Text(*b"\xa9gen"),
            TagField::Compilation => Mp4Target::Flag(*b"cpil"),
            TagField::TrackNumber => Mp4Target::Pair(*b"trkn", Slot::Number),
            TagField::TrackTotal => Mp4Target::Pair(*b"trkn", Slot::Total),
            TagField::DiscNumber => Mp4Target::Pair(*b"disk", Slot::Number),
            TagField::DiscTotal => Mp4Target::Pair(*b"disk", Slot::Total),
            TagField::DiscSubtitle => Mp4Target::Freeform("DISCSUBTITLE"),
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
