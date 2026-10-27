//! Library view DTOs plus View -> Subsonic converters.
//! v2: the Subsonic models (`to_*`, `mime_for`, `iso`).
//!
//! These are minimal local shapes carrying exactly the fields the wire
//! needs; the store fills them from the real library.

use std::path::{Component, Path};

use super::ids::{IdKind, cover_id, encode};
use super::models::{
    SAlbumID3, SArtist, SArtistID3, SChild, SDiscTitle, SGenre, SItemDate, SItemGenre, SReplayGain,
    UnreadAlbumTags, UnreadTags,
};

/// MIME per extension, from the stream routes' table (unknown ->
/// octet-stream).
pub fn mime_for(file_format: &str) -> &'static str {
    droppedneedle_media::stream::routes::content_type_for_extension(file_format)
        .unwrap_or("application/octet-stream")
}

/// Unix seconds -> `YYYY-MM-DDTHH:MM:SSZ` (v2 `iso`).
pub fn iso(unix_seconds: Option<i64>) -> Option<String> {
    unix_seconds.map(|ts| {
        let days = ts.div_euclid(86_400);
        let secs = ts.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days + 719_468);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    })
}

/// Howard Hinnant's days-to-civil for the ISO formatter above.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y += i64::from(m <= 2);
    (y, m, d)
}

/// A UTC timestamp cut to whole seconds, as `...Z` (v2 `played_iso`). It
/// reads the `YYYY-MM-DDTHH:MM:SSZ` form the store hands over and drops a
/// fraction. It does not apply an offset: a time that is not UTC would
/// keep its local clock. Garbage becomes `None` rather than failing the
/// whole response.
pub fn played_iso(value: Option<&str>) -> Option<String> {
    let value = value?;
    let upper = value.trim().to_uppercase().replace(' ', "T");
    let (date, time) = upper.split_once('T')?;
    if date.len() == 10 && time.len() >= 8 && time.is_char_boundary(8) {
        Some(format!("{date}T{}Z", &time[..8]))
    } else {
        None
    }
}

/// Genre slug: lowercase, runs of non-alphanumerics become `-`.
pub fn genre_slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Parse `YYYY[-MM[-DD]]` into an item date (v2 `_item_date`).
pub fn item_date(value: Option<&str>) -> Option<SItemDate> {
    let value = value?;
    let mut parts = value.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: Option<i64> = parts.next().map(str::parse).transpose().ok()?;
    let day: Option<i64> = parts.next().map(str::parse).transpose().ok()?;
    if !(1..=9999).contains(&year) {
        return None;
    }
    if month.is_some_and(|m| !(1..=12).contains(&m)) {
        return None;
    }
    if day.is_some_and(|d| !(1..=31).contains(&d)) {
        return None;
    }
    Some(SItemDate { year, month, day })
}

/// A song's `path`: the file's path under its own library folder, with `/`
/// between the parts. The folder's location on the host is never part of
/// it, and a stored value that is not made of plain names (a leading `/`,
/// a `..`) is not sent at all.
pub fn song_path(relative: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// An item's ratings, from 1 to 5.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRating {
    /// The caller's rating, None when they have not rated it.
    pub own: Option<u8>,
    /// The mean of every user's rating.
    pub average: f64,
}

impl ViewRating {
    /// The caller's rating, None when they have not rated it.
    fn own(rating: Option<&Self>) -> Option<i64> {
        rating.and_then(|rating| rating.own).map(i64::from)
    }

    /// The mean of every user's rating, None when nobody rated it.
    fn average(rating: Option<&Self>) -> Option<f64> {
        rating.map(|rating| rating.average)
    }
}

/// Artist row seen by the converters.
#[derive(Debug, Clone, Default)]
pub struct ViewArtist {
    /// Artist mbid (internal id).
    pub artist_mbid: String,
    /// Display name.
    pub name: String,
    /// Album count.
    pub album_count: Option<i64>,
    /// Streamable tracks credited.
    pub track_count: Option<i64>,
    /// Starred unix seconds.
    pub starred_at: Option<i64>,
    /// The caller's and everyone's rating, when anyone rated it.
    pub rating: Option<ViewRating>,
    /// Projected MusicBrainz id, if identity projection ran.
    pub musicbrainz_artist_id: Option<String>,
    /// True when provider identity was projected.
    pub provider_identity_projected: bool,
}

/// Album row seen by the converters.
#[derive(Debug, Clone, Default)]
pub struct ViewAlbum {
    /// Release-group mbid (internal id).
    pub rg_mbid: String,
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist_name: Option<String>,
    /// Album artist mbid.
    pub artist_mbid: Option<String>,
    /// Track count.
    pub track_count: Option<i64>,
    /// Total duration seconds.
    pub total_duration_seconds: Option<f64>,
    /// Play count.
    pub play_count: Option<i64>,
    /// Date added unix seconds.
    pub date_added: Option<i64>,
    /// Starred unix seconds.
    pub starred_at: Option<i64>,
    /// The caller's and everyone's rating, when anyone rated it.
    pub rating: Option<ViewRating>,
    /// Release year.
    pub year: Option<i64>,
    /// Genre name.
    pub genre: Option<String>,
    /// Compilation flag.
    pub is_compilation: Option<bool>,
    /// Projected MusicBrainz id: the release, or the release group of an
    /// accepted match with no known release.
    pub musicbrainz_album_id: Option<String>,
    /// Projected MusicBrainz artist id.
    pub musicbrainz_artist_id: Option<String>,
    /// True when provider identity was projected.
    pub provider_identity_projected: bool,
    /// Last-played ISO timestamp.
    pub played_at: Option<String>,
    /// Sort name.
    pub sort_name: Option<String>,
    /// Original release date `YYYY[-MM[-DD]]`.
    pub original_release_date: Option<String>,
    /// This edition's release date `YYYY[-MM[-DD]]`.
    pub release_date: Option<String>,
    /// Release types (defaults to ["album"], or ["compilation"]).
    pub release_types: Option<Vec<String>>,
    /// (disc, title) pairs.
    pub disc_titles: Vec<(i64, String)>,
    /// The cover version its cover id carries.
    pub cover_version: Option<String>,
}

/// Track row seen by the converters.
#[derive(Debug, Clone, Default)]
pub struct ViewTrack {
    /// File id (internal id).
    pub file_id: String,
    /// Track title.
    pub title: String,
    /// Release-group mbid.
    pub rg_mbid: Option<String>,
    /// Album title.
    pub album_title: Option<String>,
    /// Artist name.
    pub artist_name: String,
    /// Artist mbid.
    pub artist_mbid: Option<String>,
    /// Album artist name.
    pub album_artist_name: Option<String>,
    /// Album artist mbid.
    pub album_artist_mbid: Option<String>,
    /// Track number (0 sorts as missing).
    pub track_number: i64,
    /// Disc number (0 sorts as missing).
    pub disc_number: i64,
    /// Release year.
    pub year: Option<i64>,
    /// Genre (`;`-separated for the genres list).
    pub genre: Option<String>,
    /// File size bytes (0 sorts as missing).
    pub file_size_bytes: i64,
    /// File format suffix.
    pub file_format: Option<String>,
    /// Path under the track's own library folder.
    pub relative_path: Option<String>,
    /// Duration seconds.
    pub duration_seconds: f64,
    /// Bitrate kbps.
    pub bitrate: Option<i64>,
    /// Created unix seconds.
    pub created_at: Option<i64>,
    /// Starred unix seconds.
    pub starred_at: Option<i64>,
    /// The caller's and everyone's rating, when anyone rated it.
    pub rating: Option<ViewRating>,
    /// Play count.
    pub play_count: Option<i64>,
    /// Bit depth.
    pub bit_depth: Option<i64>,
    /// Sample rate Hz.
    pub sample_rate: Option<i64>,
    /// Channels (None renders as 2).
    pub channels: Option<i64>,
    /// Projected MusicBrainz recording id.
    pub musicbrainz_recording_id: Option<String>,
    /// Projected MusicBrainz artist id.
    pub musicbrainz_artist_id: Option<String>,
    /// Projected MusicBrainz album-artist id.
    pub musicbrainz_album_artist_id: Option<String>,
    /// Recording mbid fallback.
    pub recording_mbid: Option<String>,
    /// True when provider identity was projected.
    pub provider_identity_projected: bool,
    /// Last-played ISO timestamp.
    pub played_at: Option<String>,
    /// Sort name.
    pub sort_name: Option<String>,
    /// Replay gain track gain dB.
    pub replaygain_track_gain: Option<f64>,
    /// Replay gain album gain dB.
    pub replaygain_album_gain: Option<f64>,
    /// Replay gain track peak.
    pub replaygain_track_peak: Option<f64>,
    /// Replay gain album peak.
    pub replaygain_album_peak: Option<f64>,
    /// The cover version of its album's cover id.
    pub cover_version: Option<String>,
}

/// Genre row seen by the converters.
#[derive(Debug, Clone, Default)]
pub struct ViewGenre {
    /// Genre name.
    pub name: String,
    /// Song count.
    pub song_count: i64,
    /// Album count.
    pub album_count: i64,
}

/// What an artist is credited as, in a fixed order: `albumartist` with an
/// album of its own, `artist` with a credited track, and `maincredit` with
/// either. Navidrome's order differs from answer to answer.
pub fn artist_roles(view: &ViewArtist) -> Vec<String> {
    let leads = view.album_count.is_some_and(|count| count > 0);
    let plays = view.track_count.is_some_and(|count| count > 0);
    let mut roles = Vec::new();
    if leads {
        roles.push("albumartist".to_owned());
    }
    if plays {
        roles.push("artist".to_owned());
    }
    if leads || plays {
        roles.push("maincredit".to_owned());
    }
    roles
}

/// A listed item's `musicBrainzId`: its id, else an empty string as
/// Navidrome sends, or `fallback` where no identity was projected.
fn musicbrainz_id(
    id: Option<&String>,
    projected: bool,
    fallback: Option<&String>,
) -> Option<String> {
    match (id, projected) {
        (Some(id), _) => Some(id.clone()),
        (None, true) => Some(String::new()),
        (None, false) => fallback.cloned(),
    }
}

/// Artist -> ID3 artist (v2 `to_artist_id3`), with its picture link.
pub fn to_artist_id3(view: &ViewArtist, image_url: Option<String>) -> SArtistID3 {
    let aid = encode(IdKind::Artist, &view.artist_mbid);
    SArtistID3 {
        id: aid.clone(),
        name: view.name.clone(),
        coverArt: Some(aid),
        albumCount: view.album_count,
        userRating: ViewRating::own(view.rating.as_ref()),
        averageRating: ViewRating::average(view.rating.as_ref()),
        artistImageUrl: image_url,
        starred: iso(view.starred_at),
        musicBrainzId: musicbrainz_id(
            view.musicbrainz_artist_id.as_ref(),
            view.provider_identity_projected,
            Some(&view.artist_mbid),
        ),
        sortName: Some(view.name.clone()),
        roles: Some(artist_roles(view)),
        album: None,
    }
}

/// Artist -> file-structure artist (v2 `to_artist_file`), with its picture
/// link.
pub fn to_artist_file(view: &ViewArtist, image_url: Option<String>) -> SArtist {
    let aid = encode(IdKind::Artist, &view.artist_mbid);
    SArtist {
        id: aid.clone(),
        name: view.name.clone(),
        starred: iso(view.starred_at),
        coverArt: Some(aid),
        artistImageUrl: image_url,
    }
}

/// The album artist as the one-entry list `artists` carries.
fn album_artists(view: &ViewAlbum) -> Option<Vec<SArtistID3>> {
    view.artist_mbid.as_ref().map(|mbid| {
        vec![SArtistID3 {
            id: encode(IdKind::Artist, mbid),
            name: view
                .artist_name
                .clone()
                .unwrap_or_else(|| "Unknown Artist".to_owned()),
            musicBrainzId: view
                .musicbrainz_artist_id
                .clone()
                .or_else(|| (!view.provider_identity_projected).then(|| mbid.clone())),
            ..SArtistID3::default()
        }]
    })
}

fn album_genres(view: &ViewAlbum) -> Vec<SItemGenre> {
    view.genre
        .iter()
        .map(|genre| SItemGenre {
            name: genre.clone(),
        })
        .collect()
}

/// Album -> ID3 album (v2 `to_album_id3`).
pub fn to_album_id3(view: &ViewAlbum) -> SAlbumID3 {
    let alid = encode(IdKind::Album, &view.rg_mbid);
    let artists = album_artists(view);
    let release_types = view.release_types.clone().unwrap_or_else(|| {
        if view.is_compilation == Some(true) {
            vec!["compilation".to_owned()]
        } else {
            vec!["album".to_owned()]
        }
    });
    SAlbumID3 {
        id: alid,
        name: view.title.clone(),
        artist: view.artist_name.clone(),
        artistId: view
            .artist_mbid
            .as_ref()
            .map(|mbid| encode(IdKind::Artist, mbid)),
        coverArt: Some(cover_id(&view.rg_mbid, view.cover_version.as_deref())),
        songCount: view.track_count,
        duration: view.total_duration_seconds.map(|d| d.round() as i64),
        playCount: view.play_count,
        created: iso(view.date_added),
        starred: iso(view.starred_at),
        year: view.year,
        genre: view.genre.clone(),
        isCompilation: view.is_compilation,
        musicBrainzId: musicbrainz_id(
            view.musicbrainz_album_id.as_ref(),
            view.provider_identity_projected,
            Some(&view.rg_mbid),
        ),
        played: played_iso(view.played_at.as_deref()),
        genres: album_genres(view),
        artists,
        displayArtist: view.artist_name.clone(),
        releaseTypes: Some(release_types),
        sortName: Some(view.sort_name.clone().unwrap_or_else(|| view.title.clone())),
        originalReleaseDate: item_date(view.original_release_date.as_deref()),
        releaseDate: item_date(view.release_date.as_deref()),
        discTitles: view
            .disc_titles
            .iter()
            .map(|(disc, title)| SDiscTitle {
                disc: *disc,
                title: title.clone(),
                coverArt: None,
            })
            .collect(),
        userRating: ViewRating::own(view.rating.as_ref()).unwrap_or(0),
        averageRating: ViewRating::average(view.rating.as_ref()),
        unreadTags: UnreadAlbumTags::default(),
        song: None,
    }
}

/// Track -> song child (v2 `to_child`). `transcode_hint` carries
/// (content type, suffix) when transcoding is on and ffmpeg is present.
pub fn to_child(view: &ViewTrack, transcode_hint: Option<(&str, &str)>) -> SChild {
    let alid = view.rg_mbid.as_ref().map(|rg| encode(IdKind::Album, rg));
    let track_artist = view.artist_mbid.as_ref().map(|mbid| SArtistID3 {
        id: encode(IdKind::Artist, mbid),
        name: view.artist_name.clone(),
        musicBrainzId: view
            .musicbrainz_artist_id
            .clone()
            .or_else(|| (!view.provider_identity_projected).then(|| mbid.clone())),
        ..SArtistID3::default()
    });
    let album_artist = view.album_artist_mbid.as_ref().map(|mbid| SArtistID3 {
        id: encode(IdKind::Artist, mbid),
        name: view
            .album_artist_name
            .clone()
            .unwrap_or_else(|| "Unknown Artist".to_owned()),
        musicBrainzId: view
            .musicbrainz_album_artist_id
            .clone()
            .or_else(|| (!view.provider_identity_projected).then(|| mbid.clone())),
        ..SArtistID3::default()
    });
    let genres: Vec<SItemGenre> = view
        .genre
        .as_deref()
        .unwrap_or("")
        .split(';')
        .filter_map(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty()).then(|| SItemGenre {
                name: trimmed.to_owned(),
            })
        })
        .collect();
    SChild {
        id: encode(IdKind::Track, &view.file_id),
        isDir: false,
        title: view.title.clone(),
        parent: alid.clone(),
        album: view.album_title.clone(),
        artist: Some(view.artist_name.clone()),
        track: (view.track_number != 0).then_some(view.track_number),
        year: view.year,
        genre: view.genre.clone(),
        coverArt: view
            .rg_mbid
            .as_ref()
            .map(|rg| cover_id(rg, view.cover_version.as_deref())),
        size: (view.file_size_bytes != 0).then_some(view.file_size_bytes),
        contentType: Some(mime_for(view.file_format.as_deref().unwrap_or("")).to_owned()),
        suffix: view.file_format.clone(),
        transcodedContentType: transcode_hint.map(|(ct, _)| ct.to_owned()),
        transcodedSuffix: transcode_hint.map(|(_, suffix)| suffix.to_owned()),
        duration: Some(view.duration_seconds.round() as i64),
        bitRate: view.bitrate,
        path: view.relative_path.as_deref().and_then(song_path),
        discNumber: (view.disc_number != 0).then_some(view.disc_number),
        created: iso(view.created_at),
        starred: iso(view.starred_at),
        albumId: alid,
        artistId: view
            .artist_mbid
            .as_ref()
            .map(|mbid| encode(IdKind::Artist, mbid)),
        type_: Some("music".to_owned()),
        userRating: ViewRating::own(view.rating.as_ref()),
        averageRating: ViewRating::average(view.rating.as_ref()),
        playCount: view.play_count,
        mediaType: Some("song".to_owned()),
        bitDepth: Some(view.bit_depth.unwrap_or(0)),
        samplingRate: view.sample_rate,
        channelCount: Some(view.channels.unwrap_or(2)),
        musicBrainzId: musicbrainz_id(
            view.musicbrainz_recording_id.as_ref(),
            view.provider_identity_projected,
            view.recording_mbid.as_ref(),
        ),
        played: played_iso(view.played_at.as_deref()),
        sortName: Some(view.sort_name.clone().unwrap_or_else(|| view.title.clone())),
        genres: Some(genres),
        artists: track_artist.map(|artist| vec![artist]),
        displayArtist: (!view.artist_name.is_empty()).then(|| view.artist_name.clone()),
        albumArtists: album_artist.map(|artist| vec![artist]),
        displayAlbumArtist: view.album_artist_name.clone(),
        replayGain: Some(SReplayGain {
            trackGain: view.replaygain_track_gain,
            albumGain: view.replaygain_album_gain,
            trackPeak: view.replaygain_track_peak,
            albumPeak: view.replaygain_album_peak,
        }),
        name: None,
        songCount: None,
        unreadTags: Some(UnreadTags::default()),
    }
}

/// Album -> file-structure child (v2 `to_album_child`): an album as the
/// older folder-style lists show it. The audio facts are a song's, so an
/// album carries Navidrome's zero for each and an empty replay gain.
pub fn to_album_child(view: &ViewAlbum) -> SChild {
    let alid = encode(IdKind::Album, &view.rg_mbid);
    let artist_id = view
        .artist_mbid
        .as_ref()
        .map(|mbid| encode(IdKind::Artist, mbid));
    let artists = album_artists(view);
    SChild {
        id: alid,
        isDir: true,
        title: view.title.clone(),
        parent: artist_id.clone(),
        album: Some(view.title.clone()),
        artist: view.artist_name.clone(),
        artistId: artist_id,
        coverArt: Some(cover_id(&view.rg_mbid, view.cover_version.as_deref())),
        year: view.year,
        genre: view.genre.clone(),
        created: iso(view.date_added),
        starred: iso(view.starred_at),
        duration: view.total_duration_seconds.map(|d| d.round() as i64),
        mediaType: Some("album".to_owned()),
        bitDepth: Some(0),
        samplingRate: Some(0),
        channelCount: Some(0),
        musicBrainzId: musicbrainz_id(
            view.musicbrainz_album_id.as_ref(),
            view.provider_identity_projected,
            Some(&view.rg_mbid),
        ),
        sortName: Some(view.sort_name.clone().unwrap_or_else(|| view.title.clone())),
        genres: Some(album_genres(view)),
        artists: artists.clone(),
        displayArtist: view.artist_name.clone(),
        albumArtists: artists,
        displayAlbumArtist: view.artist_name.clone(),
        replayGain: Some(SReplayGain::default()),
        name: Some(view.title.clone()),
        songCount: view.track_count,
        unreadTags: Some(UnreadTags::default()),
        ..SChild::default()
    }
}

/// Genre -> wire genre (v2 `to_genre`).
pub fn to_genre(view: &ViewGenre) -> SGenre {
    SGenre {
        value: view.name.clone(),
        songCount: view.song_count,
        albumCount: view.album_count,
    }
}
