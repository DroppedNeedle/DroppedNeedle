//! The tags a file gets from its identified release, the way Picard
//! fills them: credits, numbering, dates, release facts, and every
//! MusicBrainz id. Keys are [`TagField`] names; empty facts are left out.
//! MusicBrainz has no title or album sort names, so those stay as the
//! file has them.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use super::fields::TagField;
use crate::matching::model::{
    ArtistAlias, ArtistName, ArtistNames, CreditedArtist, RecordingRelations, Release,
    ReleaseTrack, RoleCredit, VARIOUS_ARTISTS_MBID,
};
use droppedneedle_kernel::runtime_config::sections::{ArtistCreditSettings, ArtistStandardization};

/// How artist names are written, the way Picard 2.13.3 does it
/// (`picard/mbjson.py`, `artist_credit_from_node`,
/// `_translate_artist_node`, `_locales_from_aliases` and
/// `_relations_to_metadata_target_type_artist`): the credited name or the
/// artist's own MusicBrainz name ("standardise artist credits"), and,
/// when translating, the artist's primary alias in the first preferred
/// locale MusicBrainz has one for.
#[derive(Debug, Clone, Default)]
pub struct ArtistNaming<'a> {
    standardize: bool,
    translate: bool,
    /// Lower case, `-` read as `_`.
    locales: Vec<String>,
    names: Option<&'a ArtistNames>,
}

/// MusicBrainz alias types Picard scores ("Artist name", "Legal name").
const ALIAS_ARTIST_NAME: &str = "894afba6-2816-3c24-8072-eadb66bd04bc";
const ALIAS_LEGAL_NAME: &str = "d4dcd0c0-b341-3612-a332-c0ce797b25cf";

impl<'a> ArtistNaming<'a> {
    /// The names as credited, with the artists' own sort names: what
    /// files are tagged with when no profile says otherwise.
    pub fn credited() -> Self {
        Self::default()
    }

    /// A profile's artist-credit settings over a release's fetched names
    /// (`None` when they were not fetched).
    pub fn new(settings: &ArtistCreditSettings, names: Option<&'a ArtistNames>) -> Self {
        Self {
            standardize: settings.standardization == ArtistStandardization::Canonical,
            translate: settings.translate_names,
            locales: settings
                .preferred_locales
                .iter()
                .map(|locale| locale_key(locale))
                .filter(|locale| !locale.is_empty())
                .collect(),
            names,
        }
    }

    /// The name and sort name one credited artist is written under, or
    /// `None` when that needs the artist's MusicBrainz names and they are
    /// not in the fetched part (or it was not fetched).
    pub fn resolve(
        &self,
        artist_id: &str,
        credited: &str,
        own_sort: Option<&str>,
    ) -> Option<(String, Option<String>)> {
        let various = artist_id.eq_ignore_ascii_case(VARIOUS_ARTISTS_MBID);
        let translate = self.translate && !various;
        if !self.standardize && !translate {
            return Some((credited.to_owned(), own_sort.map(str::to_owned)));
        }
        let artist = self
            .names?
            .artists
            .get(artist_id.to_ascii_lowercase().as_str())?;
        let (translated, sort) = if translate {
            self.translate_artist(artist)
        } else {
            (artist.name.clone(), None)
        };
        let sort = sort
            .or_else(|| own_sort.map(str::to_owned))
            .or_else(|| artist.sort_name.clone());
        let name = if translated != artist.name {
            translated
        } else if !self.standardize && !credited.is_empty() {
            credited.to_owned()
        } else {
            artist.name.clone()
        };
        Some((name, sort))
    }

    /// A relationship credit's name. MusicBrainz sends no aliases for
    /// relationship artists (BrainzMash ws/2, 2026-10-15), so, as Picard
    /// does then, translating reverses the sort name; otherwise the
    /// credited-as name in credited style, the artist's own in canonical.
    fn relationship(&self, credit: &RoleCredit) -> String {
        if self.translate {
            let translated = credit.sort_name.as_deref().map_or_else(
                || credit.name.clone(),
                |sort| translate_from_sortname(&credit.name, sort),
            );
            if translated != credit.name {
                return translated;
            }
        }
        match &credit.credited {
            Some(credited) if !self.standardize => credited.clone(),
            _ => credit.name.clone(),
        }
    }

    /// Picard's translation: the winning alias's name and sort name, else
    /// the sort name reversed for a name not in Latin script.
    fn translate_artist(&self, artist: &ArtistName) -> (String, Option<String>) {
        if let Some(alias) = artist
            .aliases
            .as_deref()
            .and_then(|aliases| pick_alias(aliases, &self.locales))
        {
            return (alias.name.clone(), alias.sort_name.clone());
        }
        let name = artist.sort_name.as_deref().map_or_else(
            || artist.name.clone(),
            |sort| translate_from_sortname(&artist.name, sort),
        );
        (name, artist.sort_name.clone())
    }
}

/// A locale compared without case, with `-` read as `_` ("en-GB" is
/// MusicBrainz's `en_GB`).
fn locale_key(locale: &str) -> String {
    locale.trim().to_ascii_lowercase().replace('-', "_")
}

/// Picard's alias choice (`_locales_from_aliases`): only primary aliases
/// with a locale count, each under its full locale and its root (the part
/// before `_`). Between aliases sharing a key the higher score wins and
/// the first stays on a tie: locale 0.8 (0.4 for a regional one under
/// its root) and type 0.8 for an artist name, 0.5 for a legal name, 0
/// otherwise, averaged. A full-locale match for any preference beats a
/// root match for an earlier one.
fn pick_alias<'n>(aliases: &'n [ArtistAlias], preferred: &[String]) -> Option<&'n ArtistAlias> {
    let mut full: BTreeMap<String, (f64, &ArtistAlias)> = BTreeMap::new();
    let mut root: BTreeMap<String, (f64, &ArtistAlias)> = BTreeMap::new();
    for alias in aliases.iter().filter(|alias| alias.primary) {
        let Some(locale) = alias.locale.as_deref().map(locale_key) else {
            continue;
        };
        if locale.is_empty() {
            continue;
        }
        let kind = match alias.type_id.as_deref() {
            Some(ALIAS_ARTIST_NAME) => 0.8,
            Some(ALIAS_LEGAL_NAME) => 0.5,
            _ => 0.0,
        };
        let regional = if locale.contains('_') { 0.4 } else { 0.8 };
        let language = locale.split('_').next().unwrap_or_default().to_owned();
        for (map, key, score) in [
            (&mut full, locale.clone(), (0.8 + kind) / 2.0),
            (&mut root, language, (regional + kind) / 2.0),
        ] {
            if map.get(&key).is_none_or(|(best, _)| score > *best) {
                map.insert(key, (score, alias));
            }
        }
    }
    preferred
        .iter()
        .find_map(|locale| full.get(locale))
        .or_else(|| {
            preferred
                .iter()
                .find_map(|locale| root.get(locale.split('_').next().unwrap_or_default()))
        })
        .map(|(_, alias)| *alias)
}

/// A letter outside the Latin script. Picard tests "a letter whose
/// Unicode name lacks LATIN"; the script property agrees except for a few
/// Latin-script letters without LATIN in their names (ª, º and the
/// modifier letters), which count as Latin here.
static NON_LATIN_LETTER: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"[\p{L}--\p{Latin}]").ok());

/// Picard's `translate_from_sortname` (`picard/util/__init__.py`): a
/// name with a letter outside the Latin script becomes its sort name
/// reversed, split at every occurrence of the first of Picard's
/// separators found in it ("Sakamoto, Ryuichi" reads "Ryuichi
/// Sakamoto"); any other name stays as it is.
pub fn translate_from_sortname(name: &str, sort_name: &str) -> String {
    let foreign = NON_LATIN_LETTER
        .as_ref()
        .is_some_and(|letter| letter.is_match(name));
    if !foreign {
        return name.to_owned();
    }
    const SEPARATORS: [&str; 6] = [" & ", "; ", " and ", " vs. ", " with ", " y "];
    let separator = SEPARATORS
        .iter()
        .find(|separator| sort_name.contains(*separator))
        .copied()
        .unwrap_or("");
    if separator.is_empty() {
        return reverse_sortname(sort_name);
    }
    sort_name
        .split(separator)
        .map(reverse_sortname)
        .collect::<Vec<_>>()
        .join(separator)
}

/// Picard's `_reverse_sortname`: "A, B" reads "B A", "A, B, C" reads
/// "C B A", "A, B, C, D" reads "B A, D C"; anything else stays.
fn reverse_sortname(sort_name: &str) -> String {
    let chunks: Vec<&str> = sort_name.split(',').map(str::trim).collect();
    match chunks.as_slice() {
        [a, b] => format!("{b} {a}"),
        [a, b, c] => format!("{c} {b} {a}"),
        [a, b, c, d] => format!("{b} {a}, {d} {c}"),
        _ => sort_name.trim().to_owned(),
    }
}

/// One credit's artist tags under a naming: the joined text, the names,
/// and the joined sort text. `None` when a name is not fetched.
fn named_credit(
    artists: &[CreditedArtist],
    naming: &ArtistNaming<'_>,
) -> Option<(String, Vec<String>, String)> {
    let mut text = String::new();
    let mut names = Vec::with_capacity(artists.len());
    let mut sort = String::new();
    for artist in artists {
        let (name, sort_name) =
            naming.resolve(&artist.id, &artist.name, artist.sort_name.as_deref())?;
        text.push_str(&name);
        text.push_str(&artist.join);
        sort.push_str(sort_name.as_deref().unwrap_or(&name));
        sort.push_str(&artist.join);
        names.push(name);
    }
    Some((text.trim().to_owned(), names, sort.trim().to_owned()))
}

/// The full tag set for one release track, with the names as credited.
pub fn release_tags(release: &Release, track: &ReleaseTrack) -> BTreeMap<String, Vec<String>> {
    named_release_tags(release, track, &ArtistNaming::credited()).0
}

/// The full tag set for one release track under a naming, and the artist
/// fields left out because a name they need was not fetched.
pub fn named_release_tags(
    release: &Release,
    track: &ReleaseTrack,
    naming: &ArtistNaming<'_>,
) -> (BTreeMap<String, Vec<String>>, Vec<TagField>) {
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

    let mut unresolved = Vec::new();
    put(TagField::Title, vec![track.title.clone()]);
    match named_credit(track_artists, naming) {
        Some((text, names, sort)) => {
            put(TagField::Artist, vec![text]);
            put(TagField::Artists, names);
            put(TagField::ArtistSort, vec![sort]);
        }
        None => unresolved.extend([TagField::Artist, TagField::Artists, TagField::ArtistSort]),
    }
    put(TagField::Album, vec![release.title.clone()]);
    match named_credit(&release.artists, naming) {
        Some((text, names, sort)) => {
            put(TagField::AlbumArtist, vec![text]);
            put(TagField::AlbumArtists, names);
            put(TagField::AlbumArtistSort, vec![sort]);
        }
        None => unresolved.extend([
            TagField::AlbumArtist,
            TagField::AlbumArtists,
            TagField::AlbumArtistSort,
        ]),
    }
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
    (tags, unresolved)
}

fn ids(artists: &[CreditedArtist]) -> Vec<String> {
    artists.iter().map(|artist| artist.id.clone()).collect()
}

/// The credit and work tags one recording's MusicBrainz relationships
/// give, keyed by [`TagField`] name; roles with no credit are left out.
///
/// - Each role lists its artists in order, each value once, named as
///   [`ArtistNaming`] says.
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
/// - With `other` ("Other roles" ticked), engineer, mixer, DJ-mixer,
///   writer and director are written, and so are the [`FOLDED_TYPES`]
///   under their main role, as Picard's `_ARTIST_REL_TYPES` does. A
///   folded performer reads `Name (orchestra)` and the like, whatever its
///   attributes. Without it, both are left out.
pub fn credit_tags(
    relations: &RecordingRelations,
    naming: &ArtistNaming<'_>,
    other: bool,
) -> BTreeMap<String, Vec<String>> {
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
        let folded = FOLDED_TYPES.contains(&credit.relation_type.as_str());
        if folded && !other {
            continue;
        }
        let field = match credit.role.as_str() {
            "composer" => TagField::Composer,
            "lyricist" => TagField::Lyricist,
            "conductor" => TagField::Conductor,
            "performer" => TagField::Performer,
            "arranger" => TagField::Arranger,
            "remixer" => TagField::Remixer,
            "producer" => TagField::Producer,
            "engineer" if other => TagField::Engineer,
            "mixer" if other => TagField::Mixer,
            "dj_mixer" if other => TagField::DjMixer,
            "writer" if other => TagField::Writer,
            "director" if other => TagField::Director,
            _ => continue,
        };
        let name = naming.relationship(credit);
        let value = if field == TagField::Performer {
            let role = match credit.relation_type.as_str() {
                "performing orchestra" => "orchestra".to_owned(),
                kind if folded => kind.to_owned(),
                kind => performer_role(&credit.attributes, kind),
            };
            if role.is_empty() {
                name
            } else {
                format!("{name} ({role})")
            }
        } else {
            name
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

/// The MusicBrainz relationship types Picard 2.13.3 writes under one of
/// the main roles (`picard/mbjson.py`, `_ARTIST_REL_TYPES`): librettist
/// as lyricist; orchestrator and instrument and vocal arranger as
/// arranger; chorus master, concertmaster and performing orchestra as a
/// performer. Written only when "Other roles" is ticked too.
pub const FOLDED_TYPES: [&str; 7] = [
    "librettist",
    "orchestrator",
    "instrument arranger",
    "vocal arranger",
    "chorus master",
    "concertmaster",
    "performing orchestra",
];

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

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::runtime_config::sections::ArtistStandardization::{
        Canonical, Credited,
    };

    /// A recorded names lookup (`server/tests/fixtures/musicbrainz`) as
    /// identification stores it, with its names part grafted on.
    fn recorded(fixture: &str) -> Release {
        let path = format!(
            "{}/../tests/fixtures/musicbrainz/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("fixture reads")).expect("parses");
        value["release-group"] = serde_json::json!({"id": "22222222-2222-4222-8222-222222222222"});
        let wire: droppedneedle_providers::musicbrainz::MbRelease =
            serde_json::from_value(value).expect("decodes");
        let names = crate::identify::sources::names_from_wire(&wire, 1);
        let mut release =
            crate::identify::sources::release_from_wire(wire, Vec::new()).expect("release");
        release.graft_names(&names);
        release
    }

    fn settings(
        standardization: ArtistStandardization,
        translate: bool,
        locales: &[&str],
    ) -> ArtistCreditSettings {
        ArtistCreditSettings {
            standardization,
            translate_names: translate,
            preferred_locales: locales.iter().map(|locale| (*locale).to_owned()).collect(),
        }
    }

    /// (artist, artists, artist sort) for a track, or for the release
    /// when `track` is `None`; `None` when the names were not fetched.
    fn credit(
        release: &Release,
        track: Option<usize>,
        settings: &ArtistCreditSettings,
    ) -> Option<(String, Vec<String>, String)> {
        let naming = ArtistNaming::new(settings, release.artist_names.as_ref());
        let (tags, unresolved) =
            named_release_tags(release, &release.tracks[track.unwrap_or(0)], &naming);
        let [text, names, sort] = if track.is_some() {
            [TagField::Artist, TagField::Artists, TagField::ArtistSort]
        } else {
            [
                TagField::AlbumArtist,
                TagField::AlbumArtists,
                TagField::AlbumArtistSort,
            ]
        };
        if unresolved.contains(&text) {
            return None;
        }
        let get = |field: TagField| tags.get(field.name()).cloned().unwrap_or_default();
        Some((get(text).join(""), get(names), get(sort).join("")))
    }

    fn row(text: &str, names: &[&str], sort: &str) -> Option<(String, Vec<String>, String)> {
        Some((
            text.to_owned(),
            names.iter().map(|name| (*name).to_owned()).collect(),
            sort.to_owned(),
        ))
    }

    /// Each Picard 2.13.3 naming rule on recorded MusicBrainz answers:
    /// Ryuichi Sakamoto + Shiro Takatani's _async surround_ (坂本龍一,
    /// credited "Ryuichi Sakamoto", has aliases; 高谷史郎 has none) and
    /// Utada Hikaru's _Fantôme_ (featured artists, regional aliases).
    #[test]
    fn artist_names_follow_picard() {
        let sakamoto = recorded("release_names_sakamoto.json");
        let utada = recorded("release_names_utada.json");
        // Track 4: 宇多田ヒカル feat. 椎名林檎; track 9: ... feat. KOHH.
        let (feat, kohh) = (Some(3), Some(8));

        // Credited style keeps the credited name and the join phrase;
        // canonical gives each artist's MusicBrainz name.
        let credited = row(
            "Ryuichi Sakamoto + Shiro Takatani",
            &["Ryuichi Sakamoto", "Shiro Takatani"],
            "Sakamoto, Ryūichi + Takatani, Shiro",
        );
        assert_eq!(
            credit(&sakamoto, None, &settings(Credited, false, &[])),
            credited
        );
        assert_eq!(
            credit(&sakamoto, None, &settings(Canonical, false, &[])),
            row(
                "坂本龍一 + 高谷史郎",
                &["坂本龍一", "高谷史郎"],
                "Sakamoto, Ryūichi + Takatani, Shiro"
            )
        );
        // `credited()` is what files were tagged with before.
        let today = named_release_tags(&sakamoto, &sakamoto.tracks[0], &ArtistNaming::credited());
        assert_eq!(today.0, release_tags(&sakamoto, &sakamoto.tracks[0]));
        assert_eq!(
            today.0[TagField::AlbumArtist.name()],
            vec!["Ryuichi Sakamoto + Shiro Takatani".to_owned()]
        );

        // Translating to [en]: the primary `en` alias with its sort name;
        // an artist with no alias and a name outside the Latin script
        // reads its sort name reversed.
        assert_eq!(
            credit(&sakamoto, None, &settings(Credited, true, &["en"])),
            row(
                "Ryūichi Sakamoto + Shiro Takatani",
                &["Ryūichi Sakamoto", "Shiro Takatani"],
                "Sakamoto, Ryūichi + Takatani, Shiro"
            )
        );
        // Non-primary `en` aliases ("Cubic U", "Ringo Sheena") never win.
        assert_eq!(
            credit(&utada, feat, &settings(Credited, true, &["en"])),
            row(
                "Hikaru Utada feat. Sheena Ringo",
                &["Hikaru Utada", "Sheena Ringo"],
                "Utada, Hikaru feat. Sheena, Ringo"
            )
        );
        // A full-locale match for a later preference beats a root match
        // for an earlier one: [en_GB, ja] picks the `ja` alias, which is
        // the MusicBrainz name, so no translation happens and credited
        // style keeps the credited name, with the alias's sort name.
        assert_eq!(
            credit(&utada, feat, &settings(Credited, true, &["en_GB", "ja"])),
            row(
                "宇多田ヒカル feat. 椎名林檎",
                &["宇多田ヒカル", "椎名林檎"],
                "うただヒカル feat. しいなりんご"
            )
        );
        assert_eq!(
            credit(&sakamoto, None, &settings(Credited, true, &["ja"])),
            row(
                "Ryuichi Sakamoto + Shiro Takatani",
                &["Ryuichi Sakamoto", "Shiro Takatani"],
                "サカモト リュウイチ + Takatani, Shiro"
            )
        );
        // [en_GB] with no `en_GB` alias goes by root: the plain `en` alias
        // beats `en_PH` under `en`...
        assert_eq!(
            credit(&utada, None, &settings(Credited, true, &["en_GB"])),
            row("Hikaru Utada", &["Hikaru Utada"], "Utada, Hikaru")
        );
        // ...and with only `en_PH` left, it is picked by its root.
        let mut regional = utada.clone();
        let names = regional.artist_names.as_mut().expect("names");
        let artist = names
            .artists
            .get_mut("b539e453-c4fe-47e3-8a07-8517eac74429")
            .expect("Utada");
        let aliases = artist.aliases.as_mut().expect("aliases");
        aliases.retain(|alias| alias.locale.as_deref() != Some("en"));
        let only_regional = aliases.clone();
        assert_eq!(
            credit(&regional, None, &settings(Credited, true, &["en_GB"])),
            row("Utada Hikaru", &["Utada Hikaru"], "Utada, Hikaru")
        );
        // A preference matches without case and with "-" for "_".
        let set_aliases = |release: &mut Release, aliases: Vec<ArtistAlias>| {
            if let Some(names) = release.artist_names.as_mut()
                && let Some(artist) = names
                    .artists
                    .get_mut("b539e453-c4fe-47e3-8a07-8517eac74429")
            {
                artist.aliases = Some(aliases);
            }
        };
        let renamed = only_regional
            .into_iter()
            .map(|mut alias| {
                if alias.locale.as_deref() == Some("en_PH") {
                    alias.locale = Some("en_GB".to_owned());
                }
                alias
            })
            .collect();
        set_aliases(&mut regional, renamed);
        assert_eq!(
            credit(&regional, None, &settings(Credited, true, &["en-gb"])),
            row("Utada Hikaru", &["Utada Hikaru"], "Utada, Hikaru")
        );
        // MusicBrainz allows one primary alias per locale, so an artist
        // name beats a legal name through the root pass.
        let alias = |name: &str, locale: &str, kind: &str| ArtistAlias {
            name: name.to_owned(),
            sort_name: Some(name.to_owned()),
            locale: Some(locale.to_owned()),
            primary: true,
            type_id: Some(kind.to_owned()),
        };
        set_aliases(
            &mut regional,
            vec![
                alias("Legal", "en_US", ALIAS_LEGAL_NAME),
                alias("Stage", "en_GB", ALIAS_ARTIST_NAME),
            ],
        );
        assert_eq!(
            credit(&regional, None, &settings(Credited, true, &["en"])),
            row("Stage", &["Stage"], "Stage")
        );

        // No alias and a Latin name: the credited name stays.
        assert_eq!(
            credit(&utada, kohh, &settings(Credited, true, &["en"])),
            row(
                "Hikaru Utada feat. KOHH",
                &["Hikaru Utada", "KOHH"],
                "Utada, Hikaru feat. KOHH"
            )
        );

        // Various Artists is never translated; canonical style gives its
        // MusicBrainz name.
        let mut various = sakamoto.clone();
        various.artists = vec![CreditedArtist {
            id: VARIOUS_ARTISTS_MBID.to_owned(),
            name: "Diverse".to_owned(),
            sort_name: Some("Various Artists".to_owned()),
            join: String::new(),
        }];
        various.artist_names = Some(ArtistNames {
            fetched_at_ms: 1,
            artists: [(
                VARIOUS_ARTISTS_MBID.to_owned(),
                ArtistName {
                    name: "Various Artists".to_owned(),
                    sort_name: Some("Various Artists".to_owned()),
                    aliases: Some(vec![alias("Verschiedene", "en", ALIAS_ARTIST_NAME)]),
                },
            )]
            .into(),
        });
        assert_eq!(
            credit(&various, None, &settings(Credited, true, &["en"])),
            row("Diverse", &["Diverse"], "Various Artists")
        );
        assert_eq!(
            credit(&various, None, &settings(Canonical, true, &["en"])),
            row("Various Artists", &["Various Artists"], "Various Artists")
        );

        // An artist missing from the names part, or no names part at all,
        // is not fetched; the credited style needs no lookup.
        let mut missing = sakamoto.clone();
        if let Some(names) = missing.artist_names.as_mut() {
            names.artists.remove("7493b80f-de18-4f9c-a7bd-08d1ea5adcff");
        }
        assert_eq!(
            credit(&missing, None, &settings(Canonical, false, &[])),
            None
        );
        missing.artist_names = None;
        assert_eq!(
            credit(&missing, None, &settings(Credited, true, &["en"])),
            None
        );
        assert_eq!(
            credit(&missing, None, &settings(Credited, false, &["en"])),
            credited
        );
    }

    /// Picard's sort-name reversal, for a name outside the Latin script.
    #[test]
    fn a_sort_name_reverses_as_picard_does() {
        for (name, sort, expected) in [
            ("坂本龍一", "Sakamoto, Ryuichi", "Ryuichi Sakamoto"),
            ("名", "A, B & C, D", "B A & D C"),
            ("名", "A, B, C", "C B A"),
            ("名", "A, B, C, D", "B A, D C"),
            ("名", "Plain", "Plain"),
            ("Björk", "Guðmundsdóttir, Björk", "Björk"),
        ] {
            assert_eq!(
                translate_from_sortname(name, sort),
                expected,
                "{name} {sort}"
            );
        }
    }

    /// Relationship credits: translating reverses a non-Latin sort name
    /// and wins; otherwise credited style writes the credited-as name and
    /// canonical the MusicBrainz name. A performer keeps its instrument.
    #[test]
    fn relationship_credits_follow_the_naming() {
        let relations = RecordingRelations {
            recording_id: "r".to_owned(),
            credits: vec![
                RoleCredit {
                    role: "performer".to_owned(),
                    relation_type: "instrument".to_owned(),
                    name: "Glenn Gould".to_owned(),
                    sort_name: Some("Gould, Glenn".to_owned()),
                    artist_id: "gould".to_owned(),
                    attributes: vec!["piano".to_owned()],
                    credited: Some("G. Gould".to_owned()),
                },
                RoleCredit {
                    role: "composer".to_owned(),
                    relation_type: "composer".to_owned(),
                    name: "坂本龍一".to_owned(),
                    sort_name: Some("Sakamoto, Ryūichi".to_owned()),
                    artist_id: "sakamoto".to_owned(),
                    attributes: Vec::new(),
                    credited: Some("Ryuichi Sakamoto".to_owned()),
                },
            ],
            works: Vec::new(),
        };
        let tags = |settings: &ArtistCreditSettings| {
            let found = credit_tags(&relations, &ArtistNaming::new(settings, None), false);
            (
                found[TagField::Performer.name()].clone(),
                found[TagField::Composer.name()].clone(),
            )
        };
        let owned = |value: &str| vec![value.to_owned()];
        assert_eq!(
            tags(&settings(Credited, false, &[])),
            (owned("G. Gould (piano)"), owned("Ryuichi Sakamoto"))
        );
        assert_eq!(
            tags(&settings(Canonical, false, &[])),
            (owned("Glenn Gould (piano)"), owned("坂本龍一"))
        );
        // Relationship artists carry no aliases: only the reversal applies.
        assert_eq!(
            tags(&settings(Credited, true, &["ja"])),
            (owned("G. Gould (piano)"), owned("Ryūichi Sakamoto"))
        );
    }
}
