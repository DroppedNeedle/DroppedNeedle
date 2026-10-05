//! Library view DTOs plus View -> Subsonic converters.
//! v2: the Subsonic models (`to_*`, `mime_for`, `iso`).
//!
//! These are minimal local shapes carrying exactly the fields the wire
//! needs; the store fills them from the real library.

use super::ids::{IdKind, encode};
use super::models::{
    SAlbumID3, SArtist, SArtistID3, SChild, SDiscTitle, SGenre, SItemDate, SItemGenre, SReplayGain,
};

/// MIME per extension (v2 `_MIME`; unknown -> octet-stream).
pub fn mime_for(file_format: &str) -> &'static str {
    match file_format.to_lowercase().as_str() {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        _ => "application/octet-stream",
    }
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

/// Normalize an ISO timestamp to UTC `...Z` (v2 `played_iso`); garbage
/// becomes `None` rather than failing the whole response.
pub fn played_iso(value: Option<&str>) -> Option<String> {
    let value = value?;
    let upper = value.trim().to_uppercase().replace(' ', "T");
    let core = upper.strip_suffix('Z').unwrap_or(&upper);
    let date_time = core.split(['+', '-']).next().unwrap_or(core);
    let (date, time) = date_time.split_once('T')?;
    if date.len() == 10 && time.len() >= 8 {
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

/// Artist row seen by the converters.
#[derive(Debug, Clone, Default)]
pub struct ViewArtist {
    /// Artist mbid (internal id).
    pub artist_mbid: String,
    /// Display name.
    pub name: String,
    /// Album count.
    pub album_count: Option<i64>,
    /// Starred unix seconds.
    pub starred_at: Option<i64>,
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
    /// Release year.
    pub year: Option<i64>,
    /// Genre name.
    pub genre: Option<String>,
    /// Compilation flag.
    pub is_compilation: Option<bool>,
    /// Projected MusicBrainz release-group id.
    pub musicbrainz_release_group_id: Option<String>,
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
    /// Release types (defaults to ["album"], or ["compilation"]).
    pub release_types: Option<Vec<String>>,
    /// (disc, title) pairs.
    pub disc_titles: Vec<(i64, String)>,
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
    /// Duration seconds.
    pub duration_seconds: f64,
    /// Bitrate kbps.
    pub bitrate: Option<i64>,
    /// Created unix seconds.
    pub created_at: Option<i64>,
    /// Starred unix seconds.
    pub starred_at: Option<i64>,
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

/// Artist -> ID3 artist (v2 `to_artist_id3`).
pub fn to_artist_id3(view: &ViewArtist) -> SArtistID3 {
    let aid = encode(IdKind::Artist, &view.artist_mbid);
    SArtistID3 {
        id: aid.clone(),
        name: view.name.clone(),
        coverArt: Some(aid),
        albumCount: view.album_count,
        starred: iso(view.starred_at),
        musicBrainzId: view
            .musicbrainz_artist_id
            .clone()
            .or_else(|| (!view.provider_identity_projected).then(|| view.artist_mbid.clone())),
        sortName: Some(view.name.clone()),
        album: None,
    }
}

/// Artist -> file-structure artist (v2 `to_artist_file`).
pub fn to_artist_file(view: &ViewArtist) -> SArtist {
    let aid = encode(IdKind::Artist, &view.artist_mbid);
    SArtist {
        id: aid.clone(),
        name: view.name.clone(),
        starred: iso(view.starred_at),
        coverArt: Some(aid),
    }
}

/// Album -> ID3 album (v2 `to_album_id3`).
pub fn to_album_id3(view: &ViewAlbum) -> SAlbumID3 {
    let alid = encode(IdKind::Album, &view.rg_mbid);
    let artists = view.artist_mbid.as_ref().map(|mbid| {
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
    });
    let release_types = view.release_types.clone().unwrap_or_else(|| {
        if view.is_compilation == Some(true) {
            vec!["compilation".to_owned()]
        } else {
            vec!["album".to_owned()]
        }
    });
    SAlbumID3 {
        id: alid.clone(),
        name: view.title.clone(),
        artist: view.artist_name.clone(),
        artistId: view
            .artist_mbid
            .as_ref()
            .map(|mbid| encode(IdKind::Artist, mbid)),
        coverArt: Some(alid),
        songCount: view.track_count,
        duration: view.total_duration_seconds.map(|d| d.round() as i64),
        playCount: view.play_count,
        created: iso(view.date_added),
        starred: iso(view.starred_at),
        year: view.year,
        genre: view.genre.clone(),
        isCompilation: view.is_compilation,
        musicBrainzId: view
            .musicbrainz_release_group_id
            .clone()
            .or_else(|| (!view.provider_identity_projected).then(|| view.rg_mbid.clone())),
        played: played_iso(view.played_at.as_deref()),
        genres: view
            .genre
            .as_ref()
            .map(|g| vec![SItemGenre { name: g.clone() }]),
        artists,
        displayArtist: view.artist_name.clone(),
        releaseTypes: Some(release_types),
        sortName: Some(view.sort_name.clone().unwrap_or_else(|| view.title.clone())),
        originalReleaseDate: item_date(view.original_release_date.as_deref()),
        discTitles: if view.disc_titles.is_empty() {
            None
        } else {
            Some(
                view.disc_titles
                    .iter()
                    .map(|(disc, title)| SDiscTitle {
                        disc: *disc,
                        title: title.clone(),
                        coverArt: None,
                    })
                    .collect(),
            )
        },
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
        coverArt: alid.clone(),
        size: (view.file_size_bytes != 0).then_some(view.file_size_bytes),
        contentType: Some(mime_for(view.file_format.as_deref().unwrap_or("")).to_owned()),
        suffix: view.file_format.clone(),
        transcodedContentType: transcode_hint.map(|(ct, _)| ct.to_owned()),
        transcodedSuffix: transcode_hint.map(|(_, suffix)| suffix.to_owned()),
        duration: Some(view.duration_seconds.round() as i64),
        bitRate: view.bitrate,
        path: None,
        discNumber: (view.disc_number != 0).then_some(view.disc_number),
        created: iso(view.created_at),
        starred: iso(view.starred_at),
        albumId: alid,
        artistId: view
            .artist_mbid
            .as_ref()
            .map(|mbid| encode(IdKind::Artist, mbid)),
        type_: Some("music".to_owned()),
        playCount: view.play_count,
        mediaType: Some("song".to_owned()),
        bitDepth: view.bit_depth,
        samplingRate: view.sample_rate,
        channelCount: Some(view.channels.unwrap_or(2)),
        musicBrainzId: view.musicbrainz_recording_id.clone().or_else(|| {
            (!view.provider_identity_projected)
                .then(|| view.recording_mbid.clone())
                .flatten()
        }),
        played: played_iso(view.played_at.as_deref()),
        sortName: Some(view.sort_name.clone().unwrap_or_else(|| view.title.clone())),
        genres: (!genres.is_empty()).then_some(genres),
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
    }
}

/// Album -> file-structure child (v2 `to_album_child`).
pub fn to_album_child(view: &ViewAlbum) -> SChild {
    let alid = encode(IdKind::Album, &view.rg_mbid);
    SChild {
        id: alid.clone(),
        isDir: true,
        title: view.title.clone(),
        album: Some(view.title.clone()),
        artist: view.artist_name.clone(),
        artistId: view
            .artist_mbid
            .as_ref()
            .map(|mbid| encode(IdKind::Artist, mbid)),
        coverArt: Some(alid),
        year: view.year,
        genre: view.genre.clone(),
        created: iso(view.date_added),
        starred: iso(view.starred_at),
        duration: view.total_duration_seconds.map(|d| d.round() as i64),
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
