//! The tags a file gets from its identified release, the way Picard
//! fills them: credits, numbering, dates, release facts, and every
//! MusicBrainz id. Keys are [`TagField`] names; empty facts are left out.

use std::collections::BTreeMap;

use super::fields::TagField;
use crate::library::matching::model::{CreditedArtist, Release, ReleaseTrack, credit_text};

/// The full tag set for one release track.
pub fn release_tags(release: &Release, track: &ReleaseTrack) -> BTreeMap<String, Vec<String>> {
    let mut tags = BTreeMap::new();
    let mut put = |field: TagField, values: Vec<String>| {
        let values: Vec<String> = values
            .into_iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect();
        if !values.is_empty() {
            tags.insert(field.name().to_owned(), values);
        }
    };
    let one = |value: Option<&str>| value.map(str::to_owned).into_iter().collect::<Vec<_>>();
    let track_artists = if track.artists.is_empty() {
        &release.artists
    } else {
        &track.artists
    };
    let medium = release
        .media
        .iter()
        .find(|medium| medium.position == track.disc);

    put(TagField::Title, vec![track.title.clone()]);
    put(TagField::Artist, vec![credit_text(track_artists)]);
    put(TagField::Artists, names(track_artists));
    put(TagField::ArtistSort, vec![sort_text(track_artists)]);
    put(TagField::Album, vec![release.title.clone()]);
    put(TagField::AlbumArtist, vec![release.artist_text()]);
    put(TagField::AlbumArtistSort, vec![sort_text(&release.artists)]);
    put(TagField::TrackNumber, vec![track.position.to_string()]);
    if let Some(medium) = medium {
        put(TagField::TrackTotal, vec![medium.track_count.to_string()]);
        put(TagField::Media, one(medium.format.as_deref()));
    }
    put(TagField::DiscNumber, vec![track.disc.to_string()]);
    put(
        TagField::DiscTotal,
        vec![release.media.len().max(1).to_string()],
    );
    put(TagField::Date, one(release.date.as_deref()));
    put(
        TagField::OriginalDate,
        one(release.original_date.as_deref()),
    );
    put(
        TagField::ReleaseStatus,
        one(release.status.as_deref().map(str::to_lowercase).as_deref()),
    );
    put(TagField::ReleaseCountry, one(release.country.as_deref()));
    put(
        TagField::ReleaseType,
        release
            .primary_type
            .iter()
            .chain(release.secondary_types.iter())
            .map(|kind| kind.to_lowercase())
            .collect(),
    );
    put(TagField::Label, release.labels.clone());
    put(TagField::CatalogNumber, release.catalog_numbers.clone());
    put(TagField::Barcode, one(release.barcode.as_deref()));
    put(TagField::Asin, one(release.asin.as_deref()));
    put(
        TagField::MusicBrainzRecordingId,
        vec![track.recording_id.clone()],
    );
    put(TagField::MusicBrainzReleaseTrackId, vec![track.id.clone()]);
    put(TagField::MusicBrainzReleaseId, vec![release.id.clone()]);
    put(
        TagField::MusicBrainzReleaseGroupId,
        vec![release.release_group_id.clone()],
    );
    put(TagField::MusicBrainzArtistId, ids(track_artists));
    put(TagField::MusicBrainzAlbumArtistId, ids(&release.artists));
    tags
}

fn names(artists: &[CreditedArtist]) -> Vec<String> {
    artists.iter().map(|artist| artist.name.clone()).collect()
}

fn ids(artists: &[CreditedArtist]) -> Vec<String> {
    artists.iter().map(|artist| artist.id.clone()).collect()
}

/// Sort names joined with the credit's join phrases ("Lanterns, The &
/// Choir, Lantern"); a missing sort name falls back to the credited name.
fn sort_text(artists: &[CreditedArtist]) -> String {
    artists
        .iter()
        .map(|artist| {
            format!(
                "{}{}",
                artist.sort_name.as_deref().unwrap_or(&artist.name),
                artist.join
            )
        })
        .collect::<String>()
        .trim()
        .to_owned()
}
