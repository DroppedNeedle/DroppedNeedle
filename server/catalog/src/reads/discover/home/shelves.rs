//! Pure shaping for the home page: rows into shelves (v2
//! `HomeSectionBuilders`), and the connect-a-service cards.

use super::super::listening::LovedTrack;
use super::library::{HomeAlbum, HomeArtist, Play};
use crate::reads::discover::listening::{ArtistRow, ListenSource};
use crate::reads::discover::listening::{LastFmChartAlbum, RankedAlbum};
use crate::reads::discover::models::{
    AlbumRef, ArtistRef, ChartSection, PromptService, SectionItem, SectionKind, ServicePrompt,
    ShelfSource, TrackRef,
};
use crate::reads::discover::page::shelves::{SHELF_SIZE, album, shelf};

/// A library album row: its own art when it has some, else the release
/// group's cover.
pub fn library_album(row: &HomeAlbum) -> AlbumRef {
    let mut item = album(
        row.release_group_mbid.as_deref(),
        &row.title,
        row.artist_name.as_deref(),
        row.artist_mbid.as_deref(),
        None,
    );
    if let Some(version) = row.art_version {
        item.cover_url = Some(format!(
            "/api/v3/library/albums/{}/artwork?size=500&v={version}",
            row.local_id
        ));
    }
    item.local_id = Some(row.local_id.clone());
    item.release_date = row.year.map(|year| year.to_string());
    item.in_library = true;
    item
}

/// A library artist row; the count is the artist's albums.
pub fn library_artist(row: &HomeArtist) -> ArtistRef {
    ArtistRef {
        name: row.name.clone(),
        musicbrainz_id: row.mbid.clone(),
        local_id: Some(row.local_id.clone()),
        image_url: None,
        listen_count: Some(row.album_count),
        in_library: true,
    }
}

/// Recently Added: the newest library albums.
pub fn recently_added(albums: &[HomeAlbum]) -> Option<ChartSection> {
    shelf(
        "Recently Added",
        SectionKind::Albums,
        albums
            .iter()
            .take(SHELF_SIZE)
            .map(|row| SectionItem::Album(library_album(row)))
            .collect(),
        Some(ShelfSource::Library),
    )
}

/// Your Albums: the same newest albums, newest release first.
pub fn your_albums(albums: &[HomeAlbum]) -> Option<ChartSection> {
    let mut sorted: Vec<&HomeAlbum> = albums.iter().collect();
    sorted.sort_by(|a, b| (b.year.unwrap_or(0), &b.title).cmp(&(a.year.unwrap_or(0), &a.title)));
    shelf(
        "Your Albums",
        SectionKind::Albums,
        sorted
            .into_iter()
            .take(SHELF_SIZE)
            .map(|row| SectionItem::Album(library_album(row)))
            .collect(),
        Some(ShelfSource::Library),
    )
}

/// Your Artists: the library artists with the most albums.
pub fn your_artists(artists: &[HomeArtist]) -> Option<ChartSection> {
    shelf(
        "Your Artists",
        SectionKind::Artists,
        artists
            .iter()
            .take(SHELF_SIZE)
            .map(|row| SectionItem::Artist(library_artist(row)))
            .collect(),
        Some(ShelfSource::Library),
    )
}

/// Trending Artists from one chart. ListenBrainz rows need an artist id
/// to link to; Last.fm rows link by name.
pub fn trending_artists(rows: &[ArtistRow], source: ListenSource) -> Option<ChartSection> {
    let items = rows
        .iter()
        .filter(|row| source == ListenSource::Lastfm || row.mbid.is_some())
        .take(SHELF_SIZE)
        .map(|row| {
            SectionItem::Artist(ArtistRef {
                musicbrainz_id: row.mbid.clone(),
                listen_count: Some(row.listen_count),
                ..ArtistRef::named(row.name.clone())
            })
        })
        .collect();
    shelf(
        "Trending Artists",
        SectionKind::Artists,
        items,
        Some(source.into()),
    )
}

fn ranked_albums(title: &str, rows: &[RankedAlbum]) -> Option<ChartSection> {
    shelf(
        title,
        SectionKind::Albums,
        rows.iter()
            .take(SHELF_SIZE)
            .map(|row| {
                SectionItem::Album(album(
                    Some(&row.album.release_group_mbid),
                    &row.album.title,
                    Some(&row.album.artist_name),
                    row.album.artist_mbid.as_deref(),
                    Some(row.listen_count),
                ))
            })
            .collect(),
        Some(ShelfSource::Listenbrainz),
    )
}

/// Popular Right Now: this week's most played albums on ListenBrainz.
pub fn popular_albums(rows: &[RankedAlbum]) -> Option<ChartSection> {
    ranked_albums("Popular Right Now", rows)
}

/// Your Top Albums from ListenBrainz (this month).
pub fn your_top_albums(rows: &[RankedAlbum]) -> Option<ChartSection> {
    ranked_albums("Your Top Albums", rows)
}

/// Your Top Albums from Last.fm (this month). Last.fm ids name releases,
/// not release groups, so rows carry Last.fm's own artwork instead.
pub fn your_top_albums_lastfm(rows: &[LastFmChartAlbum]) -> Option<ChartSection> {
    shelf(
        "Your Top Albums",
        SectionKind::Albums,
        rows.iter()
            .take(SHELF_SIZE)
            .map(|row| {
                let mut item = album(None, &row.name, Some(&row.artist_name), None, None);
                item.cover_url = Some(row.image_url.clone()).filter(|url| !url.is_empty());
                item.listen_count = Some(row.playcount);
                SectionItem::Album(item)
            })
            .collect(),
        Some(ShelfSource::Lastfm),
    )
}

/// Your Favorites: the user's loved tracks.
pub fn favorites(rows: &[LovedTrack], source: ListenSource) -> Option<ChartSection> {
    shelf(
        "Your Favorites",
        SectionKind::Tracks,
        rows.iter()
            .take(SHELF_SIZE)
            .map(|row| {
                SectionItem::Track(TrackRef {
                    title: row.name.clone(),
                    musicbrainz_id: row.recording_mbid.clone(),
                    artist_name: Some(row.artist_name.clone()),
                    artist_id: row.artist_mbid.clone(),
                    album_name: row.album_name.clone(),
                    listen_count: None,
                    listened_at: None,
                    cover_url: row.image_url.clone(),
                })
            })
            .collect(),
        Some(source.into()),
    )
}

/// Recently Played: the user's own plays in DroppedNeedle.
pub fn recently_played(plays: &[Play]) -> Option<ChartSection> {
    shelf(
        "Recently Played",
        SectionKind::Tracks,
        plays
            .iter()
            .take(SHELF_SIZE)
            .map(|play| {
                SectionItem::Track(TrackRef {
                    title: play.track_name.clone(),
                    musicbrainz_id: play.recording_mbid.clone(),
                    artist_name: Some(play.artist_name.clone()),
                    artist_id: None,
                    album_name: play.album_name.clone(),
                    listen_count: None,
                    listened_at: Some(play.played_at.clone()),
                    cover_url: play
                        .release_group_mbid
                        .as_deref()
                        .map(|group| format!("/api/v3/covers/release-group/{group}?size=250")),
                })
            })
            .collect(),
        Some(ShelfSource::Local),
    )
}

fn prompt(
    service: PromptService,
    title: &str,
    description: &str,
    icon: &str,
    color: &str,
    features: [&str; 4],
) -> ServicePrompt {
    ServicePrompt {
        service,
        title: title.to_owned(),
        description: description.to_owned(),
        icon: icon.to_owned(),
        color: color.to_owned(),
        features: features.iter().map(|text| (*text).to_owned()).collect(),
    }
}

/// The connect-a-service cards home shows: a download client when none is
/// set up, and ListenBrainz and Last.fm while neither is linked.
pub fn service_prompts(
    listenbrainz: bool,
    download_client: bool,
    lastfm: bool,
) -> Vec<ServicePrompt> {
    let mut prompts = Vec::new();
    if !download_client {
        prompts.push(prompt(
            PromptService::DownloadClient,
            "Connect a download client",
            "DroppedNeedle needs a download source (slskd for Soulseek, or SABnzbd with an indexer for Usenet) to fetch the albums and tracks you request. Add one in Settings to start downloading into your library.",
            "DL",
            "accent",
            [
                "Album and track requests",
                "Automatic downloads",
                "Direct-to-library imports",
                "Queue management",
            ],
        ));
    }
    if !listenbrainz && !lastfm {
        prompts.push(prompt(
            PromptService::Listenbrainz,
            "Connect ListenBrainz",
            "Get recommendations from your listening history, spot new releases from artists you already love, and keep an eye on your top genres. Connect Last.fm too if you want global listener stats.",
            "LB",
            "primary",
            [
                "Personalized recommendations",
                "New release alerts",
                "Listening stats",
                "Top genres",
            ],
        ));
        prompts.push(prompt(
            PromptService::Lastfm,
            "Connect Last.fm",
            "Track your listening, compare stats, and discover music that matches your taste.",
            "FM",
            "primary",
            [
                "Scrobbling",
                "Global listener stats",
                "Artist recommendations",
                "Play history",
            ],
        ));
    }
    prompts
}
