//! What the matcher compares: local files on one side, a MusicBrainz
//! release with its full tracklist on the other.
//!
//! [`Release`] is also the stored release document: identification keeps
//! it so the publisher can tag files from the release later without
//! asking MusicBrainz again.

use serde::{Deserialize, Serialize};

/// One local file, as the tags (and the fingerprint, when taken) say.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// 0 when unknown.
    pub track_number: u32,
    /// 1 when the tags say nothing.
    pub disc_number: u32,
    pub duration_secs: Option<f64>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub release_mbid: Option<String>,
    /// AcoustID recordings heard in this file's audio that some candidate
    /// release carries (see [`super::score::heard_on_candidates`]); empty
    /// when not printed or when no candidate carries any of them.
    pub fingerprint_recordings: Vec<String>,
}

/// One local album: the files the library grouped together.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalAlbum {
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub is_compilation: bool,
    pub tracks: Vec<LocalTrack>,
    /// What the tags say about the edition, album-wide.
    pub hints: EditionHints,
}

/// Edition hints file tags carry: soft signals that tell editions with the
/// same tracklist apart. Each is the value most files agree on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditionHints {
    /// `MEDIA`: `CD`, `Digital Media`, `12" Vinyl`.
    pub media: Option<String>,
    /// `BARCODE`, digits only.
    pub barcode: Option<String>,
    /// `CATALOGNUMBER`, folded.
    pub catalog_number: Option<String>,
    /// `RELEASECOUNTRY`, two letters.
    pub country: Option<String>,
    /// `TOTALDISCS`, when the files say there is more than one disc.
    pub total_discs: Option<u32>,
}

impl EditionHints {
    /// What one file's edition tags give matching: media, the barcode and
    /// catalog number keys, a two-letter country, and a disc total of more
    /// than one disc that is not below the file's own disc number. Blank
    /// tags give nothing. The scan uses the same rule to tell when a file
    /// gains a hint.
    pub fn of_file(
        media: Option<&str>,
        barcode: Option<&str>,
        catalog_number: Option<&str>,
        country: Option<&str>,
        total_discs: Option<u32>,
        disc_number: u32,
    ) -> Self {
        fn present(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }
        Self {
            media: present(media).map(str::to_owned),
            barcode: barcode.and_then(Self::barcode_key),
            catalog_number: catalog_number.and_then(Self::catalog_key),
            country: present(country)
                .filter(|code| code.len() == 2)
                .map(str::to_ascii_uppercase),
            total_discs: total_discs.filter(|total| *total >= 2 && *total >= disc_number),
        }
    }

    /// True when no hint is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Digits of a barcode without leading zeros (UPC and EAN agree).
    pub fn barcode_key(raw: &str) -> Option<String> {
        let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
        let trimmed = digits.trim_start_matches('0');
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// A catalog number without spaces, dashes or case. `[none]`, which
    /// MusicBrainz label info and Picard's tags use for "no catalog
    /// number", is none: it would name every release that says it.
    pub fn catalog_key(raw: &str) -> Option<String> {
        let key: String = raw
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect();
        (!key.is_empty() && key != "none").then_some(key)
    }
}

impl LocalAlbum {
    /// The release MBID most files agree on, when at least half carry it.
    pub fn tagged_release(&self) -> Option<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for track in &self.tracks {
            let Some(mbid) = track.release_mbid.as_deref() else {
                continue;
            };
            let mbid = mbid.trim().to_ascii_lowercase();
            match counts.iter_mut().find(|(seen, _)| *seen == mbid) {
                Some((_, count)) => *count += 1,
                None => counts.push((mbid, 1)),
            }
        }
        let (mbid, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
        (count * 2 >= self.tracks.len().max(1)).then_some(mbid)
    }

    /// The highest disc number the files claim (1 when none do).
    pub fn max_disc(&self) -> usize {
        self.tracks
            .iter()
            .map(|track| track.disc_number.max(1) as usize)
            .max()
            .unwrap_or(1)
    }
}

/// One credited artist on a release or track.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreditedArtist {
    pub id: String,
    /// The name as credited here.
    pub name: String,
    pub sort_name: Option<String>,
    /// Text joining this artist to the next (" & ", "; ", "").
    pub join: String,
}

/// Credit display text: credited names with their join phrases.
pub fn credit_text(artists: &[CreditedArtist]) -> String {
    artists
        .iter()
        .map(|artist| format!("{}{}", artist.name, artist.join))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// One medium of a release.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMedium {
    pub position: u32,
    pub format: Option<String>,
    pub title: Option<String>,
    pub track_count: u32,
}

/// One track of a release, in release order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseTrack {
    /// Release-track MBID.
    pub id: String,
    pub recording_id: String,
    pub title: String,
    pub artists: Vec<CreditedArtist>,
    /// Medium position.
    pub disc: u32,
    /// Position on the medium.
    pub position: u32,
    /// Position counted across the whole release.
    pub absolute_position: u32,
    pub length_ms: Option<u64>,
    /// The recording's ISRCs.
    #[serde(default)]
    pub isrcs: Vec<String>,
    /// The recording's MusicBrainz genres with their votes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<Genre>,
}

/// A MusicBrainz genre and its vote count (`None` when not given).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genre {
    pub name: String,
    pub count: Option<u32>,
}

/// A release with everything matching and tagging need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub release_group_id: String,
    pub title: String,
    pub artists: Vec<CreditedArtist>,
    pub date: Option<String>,
    /// The release group's first release date.
    pub original_date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub barcode: Option<String>,
    pub asin: Option<String>,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub labels: Vec<String>,
    pub catalog_numbers: Vec<String>,
    pub media: Vec<ReleaseMedium>,
    pub tracks: Vec<ReleaseTrack>,
    /// Retired MBIDs MusicBrainz redirected to this release.
    #[serde(default)]
    pub old_ids: Vec<String>,
    /// MusicBrainz's note telling this release apart from its siblings
    /// ("deluxe edition"); naming scripts read it as `album_disambiguation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disambiguation: Option<String>,
    /// The release's and its release group's MusicBrainz genres.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<Genre>,
    /// Recording and work credits, fetched on their own when a profile
    /// writes them. `None` until then; identification never fetches them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_relations: Option<ReleaseRelations>,
    /// The credited artists' own MusicBrainz names and aliases, fetched on
    /// their own when a profile writes MusicBrainz or translated names.
    /// `None` until then; identification never fetches them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_names: Option<ArtistNames>,
}

/// What [`ReleaseRelations::version`] new lookups write. Stored credits
/// below it are fetched again; until then they are used as they are.
/// 1: credits carry the name they are credited as.
/// 2: credits also carry Picard's other roles (engineer, mixer, DJ-mixer,
/// writer, director) and the types folded into the main roles.
pub const RELATIONS_VERSION: u32 = 2;

/// The credits MusicBrainz gave for a release's recordings, and when.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseRelations {
    /// Unix milliseconds.
    pub fetched_at_ms: i64,
    /// What the credits were parsed under; 0 before versions were kept.
    #[serde(default)]
    pub version: u32,
    /// By release-track id. A track missing here was not fetched; it is
    /// never read as "no credits".
    pub tracks: std::collections::BTreeMap<String, RecordingRelations>,
}

/// One recording's credits and the works it performs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingRelations {
    /// The recording the credits were fetched for.
    pub recording_id: String,
    /// The recording's credits, then each performed work's.
    pub credits: Vec<RoleCredit>,
    /// Works the recording performs (`performance` links only).
    pub works: Vec<WorkCredit>,
}

/// One credited artist relationship.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoleCredit {
    /// The tag role: composer, lyricist, conductor, performer, arranger,
    /// remixer, producer, engineer, mixer, dj_mixer, writer or director.
    /// A folded type (`librettist`, `orchestrator`, `performing
    /// orchestra`, ...) has the main role it is written under.
    pub role: String,
    /// MusicBrainz's relationship type (`instrument`, `vocal`, ...),
    /// which tells a folded credit from the main role's own.
    pub relation_type: String,
    /// The artist's MusicBrainz name, else the name credited.
    pub name: String,
    pub sort_name: Option<String>,
    pub artist_id: String,
    pub attributes: Vec<String>,
    /// The name the artist is credited as here, when it is not their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credited: Option<String>,
}

/// The credited artists' own MusicBrainz names and aliases, and when they
/// were fetched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtistNames {
    /// Unix milliseconds.
    pub fetched_at_ms: i64,
    /// By lower-case artist id.
    pub artists: std::collections::BTreeMap<String, ArtistName>,
}

/// One artist as MusicBrainz names it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtistName {
    pub name: String,
    pub sort_name: Option<String>,
    /// `None` when MusicBrainz sent no alias list for the artist.
    pub aliases: Option<Vec<ArtistAlias>>,
}

/// One of an artist's aliases.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtistAlias {
    pub name: String,
    pub sort_name: Option<String>,
    pub locale: Option<String>,
    pub primary: bool,
    pub type_id: Option<String>,
}

/// The parts of a release fetched on their own for planning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReleaseExtras {
    pub relations: Option<ReleaseRelations>,
    pub names: Option<ArtistNames>,
}

/// One performed work.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkCredit {
    pub id: String,
    pub title: Option<String>,
}

impl Release {
    pub fn artist_text(&self) -> String {
        credit_text(&self.artists)
    }

    /// True when `mbid` names this release, directly or as a merged id.
    pub fn answers_to(&self, mbid: &str) -> bool {
        self.id.eq_ignore_ascii_case(mbid)
            || self
                .old_ids
                .iter()
                .any(|old| old.eq_ignore_ascii_case(mbid))
    }

    pub fn is_various_artists(&self) -> bool {
        self.artists
            .iter()
            .any(|artist| artist.id.eq_ignore_ascii_case(VARIOUS_ARTISTS_MBID))
    }

    pub fn year(&self) -> Option<i32> {
        year_of(self.date.as_deref())
    }

    /// The credits fetched for one of this release's tracks: `None` when
    /// none were fetched for it, or they were fetched for another
    /// recording.
    pub fn track_relations(&self, track: &ReleaseTrack) -> Option<&RecordingRelations> {
        self.recording_relations
            .as_ref()?
            .tracks
            .get(&track.id)
            .filter(|found| found.recording_id.eq_ignore_ascii_case(&track.recording_id))
    }

    /// Put fetched credits on this document. Only tracks it holds, under
    /// the same recording, take them.
    pub fn graft_relations(&mut self, fetched: &ReleaseRelations) {
        let tracks = self
            .tracks
            .iter()
            .filter_map(|track| {
                let found = fetched.tracks.get(&track.id)?;
                found
                    .recording_id
                    .eq_ignore_ascii_case(&track.recording_id)
                    .then(|| (track.id.clone(), found.clone()))
            })
            .collect();
        self.recording_relations = Some(ReleaseRelations {
            fetched_at_ms: fetched.fetched_at_ms,
            version: fetched.version,
            tracks,
        });
    }

    /// Put fetched artist names on this document, for the artists its
    /// credits name.
    pub fn graft_names(&mut self, fetched: &ArtistNames) {
        let credited: std::collections::BTreeSet<String> = self
            .artists
            .iter()
            .chain(self.tracks.iter().flat_map(|track| &track.artists))
            .map(|artist| artist.id.to_ascii_lowercase())
            .collect();
        self.artist_names = Some(ArtistNames {
            fetched_at_ms: fetched.fetched_at_ms,
            artists: fetched
                .artists
                .iter()
                .filter(|(id, _)| credited.contains(*id))
                .map(|(id, name)| (id.clone(), name.clone()))
                .collect(),
        });
    }

    /// Put whichever fetched parts came on this document.
    pub fn graft_extras(&mut self, fetched: &ReleaseExtras) {
        if let Some(relations) = &fetched.relations {
            self.graft_relations(relations);
        }
        if let Some(names) = &fetched.names {
            self.graft_names(names);
        }
    }

    pub fn original_year(&self) -> Option<i32> {
        year_of(self.original_date.as_deref())
    }
}

/// MusicBrainz's "Various Artists" special artist.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

fn year_of(date: Option<&str>) -> Option<i32> {
    let year = date?.get(..4)?;
    year.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| year.parse().ok())
        .flatten()
}
