//! The fields the save wrapper writes: Picard's tag set, with Picard's
//! names per format (<https://picard-docs.musicbrainz.org/en/appendices/tag_mapping.html>),
//! so beets, Lidarr, Navidrome, Jellyfin, and Picard itself read them.
//!
//! Picard tag mapping, read 2026-10-14, for the fields beyond the
//! release facts (the code below is the full list):
//!
//! | Field | ID3v2.4 | ID3v2.3 | Vorbis | MP4 |
//! |---|---|---|---|---|
//! | album_artists | TXXX:ALBUMARTISTS | same | ALBUMARTISTS | ----:ALBUMARTISTS |
//! | work | TXXX:WORK (never TIT1) | same | WORK | ©wrk |
//! | musicbrainz_work_id | TXXX:MusicBrainz Work Id | same | MUSICBRAINZ_WORKID | ----:MusicBrainz Work Id |
//! | movement | MVNM | MVNM | MOVEMENTNAME | ©mvn |
//! | movement_number, movement_count | MVIN n/m | MVIN n/m | MOVEMENT, MOVEMENTTOTAL | ©mvi, ©mvc (integer) |
//! | composer | TCOM | TCOM | COMPOSER | ©wrt |
//! | lyricist | TEXT | TEXT | LYRICIST | ----:LYRICIST |
//! | conductor | TPE3 | TPE3 | CONDUCTOR | ----:CONDUCTOR |
//! | performer | TMCL instrument, name | IPLS | PERFORMER=Name (instrument) | none |
//! | arranger | TIPL:arranger | IPLS:arranger | ARRANGER | none |
//! | remixer | TPE4 | TPE4 | REMIXER | ----:REMIXER |
//! | producer | TIPL:producer | IPLS:producer | PRODUCER | ----:PRODUCER |
//! | engineer | TIPL:engineer | IPLS:engineer | ENGINEER | ----:ENGINEER |
//! | mixer | TIPL:mix | IPLS:mix | MIXER | ----:MIXER |
//! | dj_mixer | TIPL:DJ-mix | IPLS:DJ-mix | DJMIXER | ----:DJMIXER |
//! | writer | TXXX:Writer | same | WRITER | ----:writer |
//! | director | TXXX:DIRECTOR | same | DIRECTOR | ©dir |
//! | comment | COMM, empty description | same | COMMENT | ©cmt |
//! | copyright | TCOP | TCOP | COPYRIGHT | cprt |
//! | encoded_by | TENC | TENC | ENCODEDBY, ENCODED-BY | ©too |
//! | acoustid_id | TXXX:Acoustid Id | same | ACOUSTID_ID | ----:Acoustid Id |
//! | acoustid_fingerprint | TXXX:Acoustid Fingerprint | same | ACOUSTID_FINGERPRINT | ----:Acoustid Fingerprint |
//!
//! The engineer, mixer, DJ-mixer, writer and director homes are Picard
//! 2.13.3's (`picard/formats/id3.py:187-221`, `vorbis.py:295`,
//! `mp4.py:85, 138-140, 289-292`, checked against tag `release-2.13.3`
//! on 2026-10-16). MP4 keeps
//! Picard's lower-case `writer`, its fallback for a name it has no
//! freeform for.

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
    Isrc,
    MusicBrainzRecordingId,
    MusicBrainzReleaseTrackId,
    MusicBrainzReleaseId,
    MusicBrainzReleaseGroupId,
    MusicBrainzArtistId,
    MusicBrainzAlbumArtistId,
    ReplayGainTrackGain,
    ReplayGainTrackPeak,
    ReplayGainAlbumGain,
    ReplayGainAlbumPeak,
    Lyrics,
    AlbumArtists,
    Work,
    MusicBrainzWorkId,
    Movement,
    MovementNumber,
    MovementCount,
    Composer,
    Lyricist,
    Conductor,
    Performer,
    Arranger,
    Remixer,
    Producer,
    Comment,
    Copyright,
    EncodedBy,
    AcoustIdId,
    AcoustIdFingerprint,
    Engineer,
    Mixer,
    DjMixer,
    Writer,
    Director,
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
    /// One value: an identifier, a word, or one piece of text.
    Single,
    /// `1` or `0`.
    Flag,
    /// One ReplayGain gain: a signed decimal followed by ` dB`.
    Gain,
    /// One ReplayGain peak: a non-negative decimal.
    Peak,
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
    /// The one unsynchronised lyrics frame (`USLT`).
    Lyrics,
    /// Name pairs in a key-value frame (`TIPL`, `TMCL`, or v2.3's `IPLS`)
    /// that this role owns.
    People(&'static str, Role),
    /// The `COMM` frame with an empty description, in any language.
    Comment,
}

/// Which pairs of a people frame a field owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Pairs whose role is this word (`producer`).
    Named(&'static str),
    /// Musician credits, `instrument` to name: every `TMCL` pair, and the
    /// `TIPL` or `IPLS` pairs whose role is not one of [`INVOLVED_ROLES`]
    /// (a v2.3 tag upgraded to v2.4 keeps them in `TIPL`; a v2.4 write
    /// moves them to `TMCL`). A pair reads as `name (instrument)`, or
    /// `name` when the instrument is empty.
    Performer,
}

/// The `TIPL` roles Picard maps to their own tags; in a v2.3 `IPLS` every
/// other role is a musician credit.
pub const INVOLVED_ROLES: &[&str] = &["arranger", "engineer", "producer", "DJ-mix", "mix"];

/// Where a field lives in an MP4 `ilst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Target {
    Text([u8; 4]),
    /// `----:com.apple.iTunes:<name>`.
    Freeform(&'static str),
    Pair([u8; 4], Slot),
    /// A one-byte boolean atom.
    Flag([u8; 4]),
    /// One integer atom (`©mvi`, `©mvc`).
    Integer([u8; 4]),
}

/// The freeform namespace Picard and iTunes use.
pub const MP4_MEAN: &str = "com.apple.iTunes";
/// The UFID owner MusicBrainz recording ids live under.
pub const MUSICBRAINZ_UFID_OWNER: &str = "http://musicbrainz.org";

impl TagField {
    pub const ALL: [TagField; 61] = [
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
        TagField::Isrc,
        TagField::MusicBrainzRecordingId,
        TagField::MusicBrainzReleaseTrackId,
        TagField::MusicBrainzReleaseId,
        TagField::MusicBrainzReleaseGroupId,
        TagField::MusicBrainzArtistId,
        TagField::MusicBrainzAlbumArtistId,
        TagField::ReplayGainTrackGain,
        TagField::ReplayGainTrackPeak,
        TagField::ReplayGainAlbumGain,
        TagField::ReplayGainAlbumPeak,
        TagField::Lyrics,
        TagField::AlbumArtists,
        TagField::Work,
        TagField::MusicBrainzWorkId,
        TagField::Movement,
        TagField::MovementNumber,
        TagField::MovementCount,
        TagField::Composer,
        TagField::Lyricist,
        TagField::Conductor,
        TagField::Performer,
        TagField::Arranger,
        TagField::Remixer,
        TagField::Producer,
        TagField::Comment,
        TagField::Copyright,
        TagField::EncodedBy,
        TagField::AcoustIdId,
        TagField::AcoustIdFingerprint,
        TagField::Engineer,
        TagField::Mixer,
        TagField::DjMixer,
        TagField::Writer,
        TagField::Director,
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
            TagField::Isrc => "isrc",
            TagField::MusicBrainzRecordingId => "musicbrainz_recording_id",
            TagField::MusicBrainzReleaseTrackId => "musicbrainz_release_track_id",
            TagField::MusicBrainzReleaseId => "musicbrainz_release_id",
            TagField::MusicBrainzReleaseGroupId => "musicbrainz_release_group_id",
            TagField::MusicBrainzArtistId => "musicbrainz_artist_id",
            TagField::MusicBrainzAlbumArtistId => "musicbrainz_album_artist_id",
            TagField::ReplayGainTrackGain => "replaygain_track_gain",
            TagField::ReplayGainTrackPeak => "replaygain_track_peak",
            TagField::ReplayGainAlbumGain => "replaygain_album_gain",
            TagField::ReplayGainAlbumPeak => "replaygain_album_peak",
            TagField::Lyrics => "lyrics",
            TagField::AlbumArtists => "album_artists",
            TagField::Work => "work",
            TagField::MusicBrainzWorkId => "musicbrainz_work_id",
            TagField::Movement => "movement",
            TagField::MovementNumber => "movement_number",
            TagField::MovementCount => "movement_count",
            TagField::Composer => "composer",
            TagField::Lyricist => "lyricist",
            TagField::Conductor => "conductor",
            TagField::Performer => "performer",
            TagField::Arranger => "arranger",
            TagField::Remixer => "remixer",
            TagField::Producer => "producer",
            TagField::Comment => "comment",
            TagField::Copyright => "copyright",
            TagField::EncodedBy => "encoded_by",
            TagField::AcoustIdId => "acoustid_id",
            TagField::AcoustIdFingerprint => "acoustid_fingerprint",
            TagField::Engineer => "engineer",
            TagField::Mixer => "mixer",
            TagField::DjMixer => "dj_mixer",
            TagField::Writer => "writer",
            TagField::Director => "director",
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
            | TagField::DiscTotal
            | TagField::MovementNumber
            | TagField::MovementCount => FieldKind::Count,
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
            | TagField::MusicBrainzReleaseGroupId
            | TagField::AcoustIdId
            | TagField::AcoustIdFingerprint
            | TagField::Comment => FieldKind::Single,
            TagField::ReplayGainTrackGain | TagField::ReplayGainAlbumGain => FieldKind::Gain,
            TagField::ReplayGainTrackPeak | TagField::ReplayGainAlbumPeak => FieldKind::Peak,
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
            TagField::Isrc => &["ISRC"],
            TagField::MusicBrainzRecordingId => &["MUSICBRAINZ_TRACKID"],
            TagField::MusicBrainzReleaseTrackId => &["MUSICBRAINZ_RELEASETRACKID"],
            TagField::MusicBrainzReleaseId => &["MUSICBRAINZ_ALBUMID"],
            TagField::MusicBrainzReleaseGroupId => &["MUSICBRAINZ_RELEASEGROUPID"],
            TagField::MusicBrainzArtistId => &["MUSICBRAINZ_ARTISTID"],
            TagField::MusicBrainzAlbumArtistId => &["MUSICBRAINZ_ALBUMARTISTID"],
            TagField::ReplayGainTrackGain => &["REPLAYGAIN_TRACK_GAIN"],
            TagField::ReplayGainTrackPeak => &["REPLAYGAIN_TRACK_PEAK"],
            TagField::ReplayGainAlbumGain => &["REPLAYGAIN_ALBUM_GAIN"],
            TagField::ReplayGainAlbumPeak => &["REPLAYGAIN_ALBUM_PEAK"],
            TagField::Lyrics => &["LYRICS", "UNSYNCEDLYRICS"],
            TagField::AlbumArtists => &["ALBUMARTISTS"],
            TagField::Work => &["WORK"],
            TagField::MusicBrainzWorkId => &["MUSICBRAINZ_WORKID"],
            TagField::Movement => &["MOVEMENTNAME"],
            TagField::MovementNumber => &["MOVEMENT"],
            TagField::MovementCount => &["MOVEMENTTOTAL"],
            TagField::Composer => &["COMPOSER"],
            TagField::Lyricist => &["LYRICIST"],
            TagField::Conductor => &["CONDUCTOR"],
            TagField::Performer => &["PERFORMER"],
            TagField::Arranger => &["ARRANGER"],
            TagField::Remixer => &["REMIXER"],
            TagField::Producer => &["PRODUCER"],
            TagField::Comment => &["COMMENT"],
            TagField::Copyright => &["COPYRIGHT"],
            TagField::EncodedBy => &["ENCODEDBY", "ENCODED-BY"],
            TagField::AcoustIdId => &["ACOUSTID_ID"],
            TagField::AcoustIdFingerprint => &["ACOUSTID_FINGERPRINT"],
            TagField::Engineer => &["ENGINEER"],
            TagField::Mixer => &["MIXER"],
            TagField::DjMixer => &["DJMIXER"],
            TagField::Writer => &["WRITER"],
            TagField::Director => &["DIRECTOR"],
        }
    }

    /// The ID3v2 home. In version 2.3 the date goes to
    /// `TYER`/`TDAT`/`TIME` and the original date to `TORY` (year only).
    /// lofty's v2.3 writer drops `TSOP`, `TSOA`, `TSOT`, and `TSST`, so in
    /// v2.3 those fields go to `TXXX` frames; `TSO2` survives and is used in
    /// both versions. Probed against lofty 0.25.4 (2026-10-14): its v2.3
    /// writer keeps `MVNM`, `MVIN`, `TCOP`, `TENC`, `TEXT`, `TPE3`, `TPE4`
    /// and `TCOM`. It reads `IPLS` as a `TIPL` and writes key-value frames
    /// back merged, so the wrapper writes the people frames (`TIPL`,
    /// `TMCL`, `IPLS`) and the comment frame from its own bytes.
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
            TagField::Isrc => Id3Target::Text("TSRC"),
            TagField::MusicBrainzRecordingId => Id3Target::Ufid(MUSICBRAINZ_UFID_OWNER),
            TagField::MusicBrainzReleaseTrackId => Id3Target::User("MusicBrainz Release Track Id"),
            TagField::MusicBrainzReleaseId => Id3Target::User("MusicBrainz Album Id"),
            TagField::MusicBrainzReleaseGroupId => Id3Target::User("MusicBrainz Release Group Id"),
            TagField::MusicBrainzArtistId => Id3Target::User("MusicBrainz Artist Id"),
            TagField::MusicBrainzAlbumArtistId => Id3Target::User("MusicBrainz Album Artist Id"),
            TagField::ReplayGainTrackGain => Id3Target::User("REPLAYGAIN_TRACK_GAIN"),
            TagField::ReplayGainTrackPeak => Id3Target::User("REPLAYGAIN_TRACK_PEAK"),
            TagField::ReplayGainAlbumGain => Id3Target::User("REPLAYGAIN_ALBUM_GAIN"),
            TagField::ReplayGainAlbumPeak => Id3Target::User("REPLAYGAIN_ALBUM_PEAK"),
            TagField::Lyrics => Id3Target::Lyrics,
            TagField::AlbumArtists => Id3Target::User("ALBUMARTISTS"),
            // Never `TIT1`, which most players read as the grouping.
            TagField::Work => Id3Target::User("WORK"),
            TagField::MusicBrainzWorkId => Id3Target::User("MusicBrainz Work Id"),
            TagField::Movement => Id3Target::Text("MVNM"),
            TagField::MovementNumber => Id3Target::Pair("MVIN", Slot::Number),
            TagField::MovementCount => Id3Target::Pair("MVIN", Slot::Total),
            TagField::Composer => Id3Target::Text("TCOM"),
            TagField::Lyricist => Id3Target::Text("TEXT"),
            TagField::Conductor => Id3Target::Text("TPE3"),
            TagField::Remixer => Id3Target::Text("TPE4"),
            TagField::Performer if v23 => Id3Target::People("IPLS", Role::Performer),
            TagField::Performer => Id3Target::People("TMCL", Role::Performer),
            TagField::Arranger if v23 => Id3Target::People("IPLS", Role::Named("arranger")),
            TagField::Arranger => Id3Target::People("TIPL", Role::Named("arranger")),
            TagField::Producer if v23 => Id3Target::People("IPLS", Role::Named("producer")),
            TagField::Producer => Id3Target::People("TIPL", Role::Named("producer")),
            TagField::Engineer if v23 => Id3Target::People("IPLS", Role::Named("engineer")),
            TagField::Engineer => Id3Target::People("TIPL", Role::Named("engineer")),
            TagField::Mixer if v23 => Id3Target::People("IPLS", Role::Named("mix")),
            TagField::Mixer => Id3Target::People("TIPL", Role::Named("mix")),
            TagField::DjMixer if v23 => Id3Target::People("IPLS", Role::Named("DJ-mix")),
            TagField::DjMixer => Id3Target::People("TIPL", Role::Named("DJ-mix")),
            TagField::Writer => Id3Target::User("Writer"),
            TagField::Director => Id3Target::User("DIRECTOR"),
            TagField::Comment => Id3Target::Comment,
            TagField::Copyright => Id3Target::Text("TCOP"),
            TagField::EncodedBy => Id3Target::Text("TENC"),
            TagField::AcoustIdId => Id3Target::User("Acoustid Id"),
            TagField::AcoustIdFingerprint => Id3Target::User("Acoustid Fingerprint"),
        }
    }

    /// The MP4 home, or `None` when the format has none (Picard and v2
    /// write no performer or arranger to MP4).
    pub fn mp4(self) -> Option<Mp4Target> {
        Some(match self {
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
            TagField::Isrc => Mp4Target::Freeform("ISRC"),
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
            TagField::ReplayGainTrackGain => Mp4Target::Freeform("replaygain_track_gain"),
            TagField::ReplayGainTrackPeak => Mp4Target::Freeform("replaygain_track_peak"),
            TagField::ReplayGainAlbumGain => Mp4Target::Freeform("replaygain_album_gain"),
            TagField::ReplayGainAlbumPeak => Mp4Target::Freeform("replaygain_album_peak"),
            TagField::Lyrics => Mp4Target::Text(*b"\xa9lyr"),
            TagField::AlbumArtists => Mp4Target::Freeform("ALBUMARTISTS"),
            TagField::Work => Mp4Target::Text(*b"\xa9wrk"),
            TagField::MusicBrainzWorkId => Mp4Target::Freeform("MusicBrainz Work Id"),
            TagField::Movement => Mp4Target::Text(*b"\xa9mvn"),
            TagField::MovementNumber => Mp4Target::Integer(*b"\xa9mvi"),
            TagField::MovementCount => Mp4Target::Integer(*b"\xa9mvc"),
            TagField::Composer => Mp4Target::Text(*b"\xa9wrt"),
            TagField::Lyricist => Mp4Target::Freeform("LYRICIST"),
            TagField::Conductor => Mp4Target::Freeform("CONDUCTOR"),
            TagField::Remixer => Mp4Target::Freeform("REMIXER"),
            TagField::Producer => Mp4Target::Freeform("PRODUCER"),
            TagField::Comment => Mp4Target::Text(*b"\xa9cmt"),
            TagField::Copyright => Mp4Target::Text(*b"cprt"),
            TagField::EncodedBy => Mp4Target::Text(*b"\xa9too"),
            TagField::AcoustIdId => Mp4Target::Freeform("Acoustid Id"),
            TagField::AcoustIdFingerprint => Mp4Target::Freeform("Acoustid Fingerprint"),
            TagField::Engineer => Mp4Target::Freeform("ENGINEER"),
            TagField::Mixer => Mp4Target::Freeform("MIXER"),
            TagField::DjMixer => Mp4Target::Freeform("DJMIXER"),
            TagField::Writer => Mp4Target::Freeform("writer"),
            TagField::Director => Mp4Target::Text(*b"\xa9dir"),
            TagField::Performer | TagField::Arranger => return None,
        })
    }
}
