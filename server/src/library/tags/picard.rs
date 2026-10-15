//! The tags a file gets from its identified release, the way Picard
//! fills them: credits, numbering, dates, release facts, and every
//! MusicBrainz id. Keys are [`TagField`] names; empty facts are left out.
//! MusicBrainz has no title or album sort names, so those stay as the
//! file has them.

use std::collections::BTreeMap;

use super::fields::TagField;
use crate::library::matching::model::{
    CreditedArtist, RecordingRelations, Release, ReleaseTrack, credit_text,
};

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
    put(TagField::AlbumArtists, names(&release.artists));
    put(TagField::AlbumArtistSort, vec![sort_text(&release.artists)]);
    put(TagField::TrackNumber, vec![track.position.to_string()]);
    if let Some(medium) = medium {
        put(TagField::TrackTotal, vec![medium.track_count.to_string()]);
        put(TagField::Media, one(medium.format.as_deref()));
        put(TagField::DiscSubtitle, one(medium.title.as_deref()));
    }
    if release.is_various_artists() {
        // Picard marks Various Artists releases as compilations.
        put(TagField::Compilation, vec!["1".to_owned()]);
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
    put(TagField::Isrc, track.isrcs.clone());
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

/// The credit and work tags one recording's MusicBrainz relationships
/// give, keyed by [`TagField`] name; roles with no credit are left out.
///
/// - Each role lists its artists in order, each value once.
/// - A performer reads `Name (instrument)`, worded as Picard 2.13.3 words
///   it (`picard/mbjson.py`, `_parse_attributes`, with its default of
///   standardized instrument names): `guest`, `additional`, `minor` and
///   `solo` lead the other attributes, several instruments read "a, b
///   and c", a vocal with no attribute is "vocals", and a performer with
///   none is the plain name (`picard/formats/vorbis.py`). One artist on
///   two instruments gives two values.
/// - The work is the performed work's title, only when the recording
///   performs exactly one (a medley's would be wrong); the work id lists
///   every performed work.
pub fn credit_tags(relations: &RecordingRelations) -> BTreeMap<String, Vec<String>> {
    let mut tags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add = |field: TagField, value: String| {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        let values = tags.entry(field.name().to_owned()).or_default();
        if !values.contains(&value) {
            values.push(value);
        }
    };
    for credit in &relations.credits {
        let field = match credit.role.as_str() {
            "composer" => TagField::Composer,
            "lyricist" => TagField::Lyricist,
            "conductor" => TagField::Conductor,
            "performer" => TagField::Performer,
            "arranger" => TagField::Arranger,
            "remixer" => TagField::Remixer,
            "producer" => TagField::Producer,
            _ => continue,
        };
        let value = if field == TagField::Performer {
            match performer_role(&credit.attributes, &credit.relation_type) {
                role if role.is_empty() => credit.name.clone(),
                role => format!("{} ({role})", credit.name),
            }
        } else {
            credit.name.clone()
        };
        add(field, value);
    }
    if let [work] = relations.works.as_slice()
        && let Some(title) = &work.title
    {
        add(TagField::Work, title.clone());
    }
    for work in &relations.works {
        add(TagField::MusicBrainzWorkId, work.id.clone());
    }
    tags
}

/// Picard's performer role text from a relationship's attributes.
fn performer_role(attributes: &[String], relation_type: &str) -> String {
    const PREFIXES: [&str; 4] = ["guest", "additional", "minor", "solo"];
    let (prefixes, nouns): (Vec<&str>, Vec<&str>) = attributes
        .iter()
        .map(String::as_str)
        .partition(|attribute| PREFIXES.contains(attribute));
    let noun = match nouns.as_slice() {
        [] if relation_type == "vocal" => "vocals".to_owned(),
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let mut parts = prefixes;
    parts.push(&noun);
    parts.join(" ").trim().to_owned()
}
