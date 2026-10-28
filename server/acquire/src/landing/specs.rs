//! Import specifications: the checks a landed download must pass.
//!
//! Lidarr's import decision engine is a list of specifications, each
//! answering accept or a reason. This is that list for the import side,
//! with typed reject codes and a disposition that decides what a
//! rejection means for failover, as pure functions over a [`Subject`]:
//!
//! - file checks run on what the probe read, before any lookup, cheapest
//!   and most decisive first: files present and whole on disk, audio
//!   present, a walk that finished, samples, wrong edition, wrong album,
//!   quality range, upgrade floor;
//! - match checks run on the closest release the matching engine found:
//!   a release found, ids that agree, a close album match, evidence of
//!   the artist for untagged files, files whose own artist agrees, files
//!   the release accounts for, the right track for a track download.
//!
//! Every check is recorded (accept, hold, or reject with its reason), so
//! the views can say why a download was held or failed over.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::matching::{FoundRelease, MatchState, MatchSummary, file_name_parts, is_various};
use super::probe::Landing;
use crate::downloads::quarantine::QuarantineReason;
use crate::reason::Reason;
use droppedneedle_library::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK};
use droppedneedle_library::matching::model::credit_text;
use droppedneedle_library::matching::score::{NAME_GATE, Support, TrackPair};
use droppedneedle_library::matching::strings::{
    artist_subset, fold, string_dist, strip_edition_words,
};

/// What a rejection means for retry and failover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// This release can never pass: fail over to the next candidate.
    Permanent,
    /// It could pass later (files still settling, MusicBrainz down).
    Temporary,
    /// Our side failed (mount, disk): never blame or blocklist the source.
    LocalFault,
}

/// One rejection: a machine code, what it means, and the blocklist reason
/// when the source itself is at fault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejection {
    pub code: Reason,
    pub disposition: Disposition,
    #[serde(skip)]
    pub quarantine: Option<QuarantineReason>,
    pub detail: String,
}

/// One check's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Passed; the note says what was left out along the way, if anything.
    Accept { note: Option<String> },
    /// Not confident enough to import: keep the files for a person.
    Hold { code: Reason, detail: String },
    /// Not importable.
    Reject(Rejection),
}

impl Verdict {
    fn accept() -> Self {
        Verdict::Accept { note: None }
    }

    fn note(text: String) -> Self {
        Verdict::Accept { note: Some(text) }
    }

    fn hold(code: Reason, detail: impl Into<String>) -> Self {
        Verdict::Hold {
            code,
            detail: detail.into(),
        }
    }

    fn reject(
        code: Reason,
        disposition: Disposition,
        quarantine: Option<QuarantineReason>,
        detail: impl Into<String>,
    ) -> Self {
        Verdict::Reject(Rejection {
            code,
            disposition,
            quarantine,
            detail: detail.into(),
        })
    }
}

/// One recorded check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub spec: &'static str,
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// What the download was for.
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub artist_name: String,
    pub album_title: String,
    pub release_group_mbid: String,
    /// The pinned edition, when the ask pinned one.
    pub release_mbid: Option<String>,
    pub year: Option<i32>,
    /// A single-track download.
    pub is_track: bool,
    /// The requested recording, for a track download.
    pub recording_mbid: Option<String>,
    pub track_title: Option<String>,
    /// The task's origin (a `DispatchOrigin` as stored text).
    pub origin: String,
    /// For a track download: the person chose to take the song from its
    /// album, so the album's track stands for the requested recording
    /// when it is the right track.
    pub album_version: bool,
    /// For a track download: the album release acquisition resolved the
    /// track to and the track's (disc, track) position on it.
    pub album_positions: Option<(String, Vec<crate::downloads::TrackPosition>)>,
    /// Byte sizes the client advertised, by the exact reported path.
    pub expected_sizes: HashMap<PathBuf, u64>,
    /// Files may still appear: a missing file is worth another pass.
    pub wait_for_files: bool,
    /// For "get missing tracks" of an edition: the (disc, position) tracks
    /// asked for. An owned track among them is replaced when the new file
    /// is strictly better, as an upgrade would.
    pub album_wanted: Vec<(u32, u32)>,
    /// The library copy the download was fetched for: its release is the
    /// one matched, the library's holdings are read from that copy only,
    /// and the files join it.
    pub planned_album: Option<crate::downloads::plan::PlannedAlbum>,
    /// A person confirmed this attempt's candidate outside the quality
    /// band: its files are kept whatever their quality.
    pub quality_waived: bool,
}

impl Target {
    /// The library copy the download was fetched for, by album id.
    pub fn planned_album_id(&self) -> Option<&str> {
        self.planned_album
            .as_ref()
            .map(|planned| planned.album_id.as_str())
    }
}

/// The quality policy a landing is judged against.
#[derive(Debug, Clone)]
pub struct QualityPolicy {
    pub quality_min: String,
    pub quality_max: String,
    /// For an upgrade: the tier the library holds, which the download
    /// must strictly beat.
    pub held_tier: Option<String>,
}

impl Default for QualityPolicy {
    fn default() -> Self {
        Self {
            quality_min: "low".to_owned(),
            quality_max: "lossless".to_owned(),
            held_tier: None,
        }
    }
}

/// Everything the checks read.
pub struct Subject<'a> {
    pub target: &'a Target,
    pub landing: &'a Landing,
    pub policy: &'a QualityPolicy,
    pub matched: Option<&'a MatchSummary>,
    /// AcoustID recordings heard in each paired file, by file index, when
    /// AcoustID was asked.
    pub heard: Option<&'a HashMap<String, Vec<String>>>,
}

/// One named specification.
pub trait ImportSpec: Send + Sync {
    fn name(&self) -> &'static str;
    fn check(&self, subject: &Subject<'_>) -> Verdict;
}

/// The file checks, in order.
pub fn file_specs() -> Vec<Box<dyn ImportSpec>> {
    vec![
        Box::new(FilesPresent),
        Box::new(SizesMatch),
        Box::new(HasAudio),
        Box::new(WalkFinished),
        Box::new(NotSample),
        Box::new(WrongEdition),
        Box::new(WrongAlbum),
        Box::new(QualityAllowed),
        Box::new(UpgradeFloor),
    ]
}

/// The match checks, in order.
pub fn match_specs() -> Vec<Box<dyn ImportSpec>> {
    vec![
        Box::new(ReleaseFound),
        Box::new(PinnedEdition),
        Box::new(IdsAgree),
        Box::new(CloseAlbumMatch),
        Box::new(ArtistEvidence),
        Box::new(OwnArtist),
        Box::new(CloseTrackMatch),
        Box::new(FilesAccountedFor),
        Box::new(RightTrack),
        Box::new(NotAlreadyImported),
    ]
}

/// Run a list of specs, recording every answer.
pub fn run(specs: &[Box<dyn ImportSpec>], subject: &Subject<'_>) -> Vec<Check> {
    specs
        .iter()
        .map(|spec| Check {
            spec: spec.name(),
            verdict: spec.check(subject),
        })
        .collect()
}

/// The first rejection, else the first hold.
pub fn worst(checks: &[Check]) -> Option<&Verdict> {
    checks
        .iter()
        .map(|check| &check.verdict)
        .find(|verdict| matches!(verdict, Verdict::Reject(_)))
        .or_else(|| {
            checks
                .iter()
                .map(|check| &check.verdict)
                .find(|verdict| matches!(verdict, Verdict::Hold { .. }))
        })
}

// ---------------------------------------------------------------------------
// File checks.
// ---------------------------------------------------------------------------

/// The client said files landed; something must be there (a local
/// fault: the peer delivered).
struct FilesPresent;

impl ImportSpec for FilesPresent {
    fn name(&self) -> &'static str {
        "files_present"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let landing = subject.landing;
        let patient = subject.target.wait_for_files;
        if landing.nothing_found() {
            return Verdict::reject(
                Reason::FilesMissing,
                if patient {
                    Disposition::Temporary
                } else {
                    Disposition::LocalFault
                },
                None,
                "downloaded files not found on the downloads mount",
            );
        }
        if !landing.missing.is_empty() {
            if patient {
                return Verdict::reject(
                    Reason::FilesMissing,
                    Disposition::Temporary,
                    None,
                    format!(
                        "{} reported file(s) not on the mount yet",
                        landing.missing.len()
                    ),
                );
            }
            return Verdict::note(format!(
                "{} reported file(s) not found",
                landing.missing.len()
            ));
        }
        Verdict::accept()
    }
}

/// Each file holds the bytes the client advertised:
/// a short file is a partial write or a stale copy on our side, so it is
/// a local fault that never blocklists the peer.
struct SizesMatch;

impl ImportSpec for SizesMatch {
    fn name(&self) -> &'static str {
        "sizes_match"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let sizes = &subject.target.expected_sizes;
        if sizes.is_empty() {
            return Verdict::accept();
        }
        let wrong = subject.landing.audio.iter().find(|file| {
            sizes
                .get(&file.path)
                .is_some_and(|expected| *expected != file.size_bytes)
        });
        match wrong {
            Some(file) => Verdict::reject(
                Reason::SizeMismatch,
                Disposition::LocalFault,
                None,
                format!(
                    "{} has {} bytes on disk, not the {} advertised",
                    file.file_name(),
                    file.size_bytes,
                    sizes.get(&file.path).copied().unwrap_or_default()
                ),
            ),
            None => Verdict::accept(),
        }
    }
}

/// The probe stops at a few thousand files; a landing that big is not one
/// album. It is a local fault: the decision is recorded and the files stay
/// where they are for a person (never copied into the held area), and the
/// source is not blocklisted.
struct WalkFinished;

impl ImportSpec for WalkFinished {
    fn name(&self) -> &'static str {
        "walk_finished"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        if subject.landing.truncated {
            return Verdict::reject(
                Reason::TooManyFiles,
                Disposition::LocalFault,
                None,
                "the download holds more files than one release can; left in place for review",
            );
        }
        Verdict::accept()
    }
}

/// At least one readable audio track. Unreadable audio is corrupt and
/// blocklists the source when nothing else is usable.
struct HasAudio;

impl ImportSpec for HasAudio {
    fn name(&self) -> &'static str {
        "has_audio"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let landing = subject.landing;
        if landing.audio.is_empty() {
            if !landing.unreadable.is_empty() {
                return Verdict::reject(
                    Reason::Corrupt,
                    Disposition::Permanent,
                    Some(QuarantineReason::Corrupt),
                    "no audio file could be read",
                );
            }
            if landing.nothing_found() {
                return Verdict::accept();
            }
            return Verdict::reject(
                Reason::NoAudio,
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                "the download holds no audio tracks",
            );
        }
        if !landing.unreadable.is_empty() {
            return Verdict::note(format!(
                "{} unreadable file(s) left out",
                landing.unreadable.len()
            ));
        }
        Verdict::accept()
    }
}

/// Sample clips are never tracks (Lidarr `NotSample`); a download of
/// nothing but samples is not the album.
struct NotSample;

impl ImportSpec for NotSample {
    fn name(&self) -> &'static str {
        "not_sample"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let samples = subject.landing.samples.len();
        if samples == 0 {
            return Verdict::accept();
        }
        if subject.landing.audio.is_empty() && subject.landing.unreadable.is_empty() {
            return Verdict::reject(
                Reason::Sample,
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                "the download is a sample, not the release",
            );
        }
        Verdict::note(format!("{samples} sample file(s) left out"))
    }
}

/// Product markers that mean a different release than a studio album.
/// Remasters and deluxe editions are the same album.
const EDITION_MARKERS: &[&str] = &[
    "live",
    "bootleg",
    "box set",
    "boxset",
    "discography",
    "discographie",
    "anthology",
    "compilation",
    "collection",
    "definitive",
    "greatest hits",
    "best of",
    "karaoke",
    "tribute",
    "instrumental",
    "a cappella",
    "acappella",
    "acapella",
    "outtake",
    "outtakes",
    "rehearsal",
    "rehearsals",
    "complete recordings",
    "complete works",
    "complete albums",
    "complete studio recordings",
];

/// The files' album tags (or, untagged, their folder) name a different
/// product than the requested album: live, bootleg, box set and the like.
/// Judged by most files, so one odd tag does not sink an album.
struct WrongEdition;

impl ImportSpec for WrongEdition {
    fn name(&self) -> &'static str {
        "wrong_edition"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let target = subject.target;
        let wanted = words(&format!("{} {}", target.album_title, target.artist_name));
        let audio = &subject.landing.audio;
        if audio.is_empty() {
            return Verdict::accept();
        }
        let mut marked = 0;
        let mut seen: Option<&'static str> = None;
        for file in audio {
            // The album tag when there is one: a peer's folder layout
            // ("Artist/Discography/...") says nothing about the files.
            let text = if file.tag.album.trim().is_empty() {
                file.path
                    .parent()
                    .and_then(|dir| dir.file_name())
                    .map(|name| words(&name.to_string_lossy()))
                    .unwrap_or_default()
            } else {
                words(&file.tag.album)
            };
            if let Some(marker) = EDITION_MARKERS
                .iter()
                .find(|marker| has_phrase(&text, marker) && !has_phrase(&wanted, marker))
            {
                marked += 1;
                seen.get_or_insert(marker);
            }
        }
        if marked * 2 > audio.len() {
            return Verdict::reject(
                Reason::WrongEdition,
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                format!(
                    "edition marker '{}' is not in the requested album",
                    seen.unwrap_or_default()
                ),
            );
        }
        Verdict::accept()
    }
}

/// Words that never make an album title different.
const NOISE_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "and",
    "disc",
    "disk",
    "cd",
    "lp",
    "ep",
    "vinyl",
    "web",
    "flac",
    "mp3",
    "remaster",
    "remastered",
    "edition",
    "deluxe",
    "expanded",
    "bonus",
    "version",
    "reissue",
    "anniversary",
    "special",
    "mono",
    "stereo",
    "hd",
    "hi",
    "res",
    "bit",
    "khz",
];

/// Most tagged files name another album by the same artist: their
/// album titles carry words the
/// requested title does not. Untagged files never count, so an untagged
/// rip falls through to matching.
struct WrongAlbum;

impl ImportSpec for WrongAlbum {
    fn name(&self) -> &'static str {
        "wrong_album"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let target = subject.target;
        if target.is_track || target.album_title.trim().is_empty() {
            return Verdict::accept();
        }
        let artist_words = words(&target.artist_name);
        let wanted = content_words(&target.album_title, &artist_words);
        let tagged: Vec<_> = subject
            .landing
            .audio
            .iter()
            .filter(|file| !file.tag.album.trim().is_empty())
            .collect();
        if tagged.is_empty() {
            return Verdict::accept();
        }
        let wrong = tagged
            .iter()
            .filter(|file| {
                let artist = file
                    .tag
                    .album_artist
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&file.tag.artist);
                let same_artist = artist.trim().is_empty()
                    || words(artist).iter().any(|word| artist_words.contains(word));
                same_artist
                    && content_words(&file.tag.album, &artist_words)
                        .iter()
                        .any(|word| !wanted.contains(word))
            })
            .count();
        if wrong * 2 > tagged.len() {
            return Verdict::reject(
                Reason::WrongAlbum,
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                format!(
                    "the files' tags name a different album than '{}'",
                    target.album_title
                ),
            );
        }
        Verdict::accept()
    }
}

/// The files' real quality must sit inside the policy band, unless a person
/// confirmed the candidate this attempt fetched outside it. Never blocklisted:
/// a peer's labels were wrong, not its files broken.
struct QualityAllowed;

impl ImportSpec for QualityAllowed {
    fn name(&self) -> &'static str {
        "quality_allowed"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(tier) = super::quality::worst(subject.landing.audio.iter().map(|f| f.tier()))
        else {
            return Verdict::accept();
        };
        let policy = subject.policy;
        if subject.target.quality_waived
            || super::quality::in_range(tier, &policy.quality_min, &policy.quality_max)
        {
            return Verdict::accept();
        }
        Verdict::reject(
            Reason::QualityRejected,
            Disposition::Permanent,
            None,
            format!(
                "files are {tier}, outside {} to {}",
                policy.quality_min, policy.quality_max
            ),
        )
    }
}

/// An upgrade must strictly beat what the library holds; equal
/// or worse is never an upgrade.
struct UpgradeFloor;

impl ImportSpec for UpgradeFloor {
    fn name(&self) -> &'static str {
        "upgrade_floor"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        if subject.target.origin != "upgrade" {
            return Verdict::accept();
        }
        let (Some(held), Some(tier)) = (
            subject.policy.held_tier.as_deref(),
            super::quality::worst(subject.landing.audio.iter().map(|f| f.tier())),
        ) else {
            return Verdict::accept();
        };
        if super::quality::beats(tier, held) {
            return Verdict::accept();
        }
        Verdict::reject(
            Reason::NotAnUpgrade,
            Disposition::Permanent,
            None,
            format!("files are {tier}, which does not beat the held {held}"),
        )
    }
}

// ---------------------------------------------------------------------------
// Match checks.
// ---------------------------------------------------------------------------

/// A release of the requested group must be found to check against.
struct ReleaseFound;

impl ImportSpec for ReleaseFound {
    fn name(&self) -> &'static str {
        "release_found"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        match subject.matched.map(|matched| &matched.state) {
            None | Some(MatchState::Matched) | Some(MatchState::Contradictory) => Verdict::accept(),
            Some(MatchState::NoCandidate) => Verdict::hold(
                Reason::NoRelease,
                "MusicBrainz has no release of the requested album to check the files against",
            ),
            Some(MatchState::Unavailable(detail)) => Verdict::reject(
                Reason::MusicbrainzUnavailable,
                Disposition::Temporary,
                None,
                format!("MusicBrainz could not be reached: {detail}"),
            ),
        }
    }
}

/// With a pinned edition, files whose own tags name another release are
/// another edition: rejected, so the next source is tried. Untagged files
/// are judged by the pinned tracklist alone.
struct PinnedEdition;

impl ImportSpec for PinnedEdition {
    fn name(&self) -> &'static str {
        "pinned_edition"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(pin) = subject.target.release_mbid.as_deref() else {
            return Verdict::accept();
        };
        let pinned = subject
            .matched
            .and_then(|matched| matched.best())
            .map(|found| &found.release);
        let is_pin = |id: &str| match pinned {
            Some(release) => release.answers_to(id),
            None => id.eq_ignore_ascii_case(pin),
        };
        let tagged: Vec<&str> = subject
            .landing
            .audio
            .iter()
            .filter_map(|file| file.tag.musicbrainz_release_id.as_deref())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .collect();
        let other: Vec<&str> = tagged.iter().copied().filter(|id| !is_pin(id)).collect();
        if !tagged.is_empty() && other.len() * 2 > tagged.len() {
            return Verdict::reject(
                Reason::NotPinnedEdition,
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                format!(
                    "the files are tagged as release {}, not the pinned edition {pin}",
                    other[0]
                ),
            );
        }
        Verdict::accept()
    }
}

/// The files' own MusicBrainz ids must not contradict every
/// candidate.
struct IdsAgree;

impl ImportSpec for IdsAgree {
    fn name(&self) -> &'static str {
        "ids_agree"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        match subject.matched.map(|matched| &matched.state) {
            Some(MatchState::Contradictory) => Verdict::hold(
                Reason::TagMismatch,
                "the files' MusicBrainz ids name a different release",
            ),
            _ => Verdict::accept(),
        }
    }
}

/// Lidarr's `CloseAlbumMatch`: the album as a whole must be close, and
/// its title and artist must agree with the release. A single found only
/// as its album's track is held with its own reason, and passes only once
/// the person chose to take it from the album.
struct CloseAlbumMatch;

impl ImportSpec for CloseAlbumMatch {
    fn name(&self) -> &'static str {
        "close_album_match"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        if found.scored.pairs.is_empty() {
            return Verdict::hold(
                Reason::WeakMatch,
                "no file pairs with a track of the release",
            );
        }
        let distance = found.scored.library_distance();
        let weak = if !found.scored.names_agree {
            "the album title or artist does not agree with the release".to_owned()
        } else if distance > ACCEPT_ALBUM {
            format!("album distance {distance:.2} is above {ACCEPT_ALBUM:.2}")
        } else {
            return Verdict::accept();
        };
        // A single whose only copy is its album's track waits for the
        // person, who can choose to take it from the album.
        if let Some(album) = album_version(subject.target, subject.landing, found) {
            if subject.target.album_version {
                return Verdict::note(format!("the track from '{album}', taken as this song"));
            }
            return Verdict::hold(
                Reason::AlbumVersionOnly,
                format!("the only copy found is the track on '{album}'"),
            );
        }
        Verdict::hold(Reason::WeakMatch, weak)
    }
}

/// For a track download whose files miss the release only because they
/// come from another one: the requested track is there, close to its
/// track, named as it (the title tag, else the file name, within the name
/// gate once edition words are set aside, so "Love Me Do" for "Love Me"
/// does not pass) and at its length ([`wrong_track`] finds nothing; a
/// track with no known length leaves the title as the guard), the files'
/// artist is the release's, and their album tag names a different
/// release. Answers that album's name.
fn album_version(target: &Target, landing: &Landing, found: &FoundRelease) -> Option<String> {
    if !target.is_track {
        return None;
    }
    let pair = found.requested_pair(target)?;
    if pair.distance > ACCEPT_TRACK || wrong_track(found, pair, landing).is_some() {
        return None;
    }
    let file = landing.audio.get(pair.local)?;
    let track = found.release.tracks.get(pair.track)?;
    let title = match file.tag.title.trim() {
        "" => file_name_parts(file).2,
        tagged => tagged.to_owned(),
    };
    if string_dist(
        &strip_edition_words(&title),
        &strip_edition_words(&track.title),
    ) > NAME_GATE
    {
        return None;
    }
    let tag = &file.tag;
    let album = tag.album.trim();
    if album.is_empty()
        || string_dist(
            &strip_edition_words(album),
            &strip_edition_words(&found.release.title),
        ) <= NAME_GATE
    {
        return None;
    }
    let artist = tag
        .album_artist
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(tag.artist.trim());
    let release_artist = found.release.artist_text();
    let same_artist = !artist.is_empty()
        && (string_dist(artist, &release_artist) <= NAME_GATE
            || artist_subset(artist, &release_artist));
    same_artist.then(|| album.to_owned())
}

/// A pair at most this far is a close fit for [`ArtistProof::Tracklist`].
const CLOSE_PAIR: f64 = 0.10;
/// Files and share of the release's tracks a close tracklist needs.
const CLOSE_FILES: usize = 3;
const CLOSE_SHARE: f64 = 0.8;

/// What shows that a download's files are by the requested artist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistProof {
    /// A file carries an artist tag, which the album gate compared.
    Tags,
    /// Every file pairs by its own MusicBrainz ids.
    Ids,
    /// The folders or file names below the downloads folder name the
    /// artist (the usual "Artist - Album" folder).
    Path,
    /// At least [`CLOSE_FILES`] files, and [`CLOSE_SHARE`] of the
    /// release's tracks paired at [`CLOSE_PAIR`] or closer.
    Tracklist,
    /// AcoustID heard the paired recording in most files it knew.
    Audio,
}

impl ArtistProof {
    fn note(self) -> &'static str {
        match self {
            ArtistProof::Tags => "the files' artist tags were compared",
            ArtistProof::Ids => "untagged, but every file carries its MusicBrainz ids",
            ArtistProof::Path => "untagged, but the folder names the artist",
            ArtistProof::Tracklist => "untagged, but the tracklist fits closely",
            ArtistProof::Audio => "untagged, but AcoustID heard the release's recordings",
        }
    }
}

/// The evidence that `found`'s files are by the requested artist, if any.
/// Untagged files take the requested names (`matching::local_album`), so
/// the album gate alone proves nothing for them. Only the paired files
/// count: a tagged extra the release does not list proves nothing about
/// the rest. `heard` is AcoustID's answer by file index, when it was asked.
pub fn artist_proof(
    target: &Target,
    landing: &Landing,
    found: &FoundRelease,
    heard: Option<&HashMap<String, Vec<String>>>,
) -> Option<ArtistProof> {
    let pairs = &found.scored.pairs;
    if pairs.is_empty() {
        return None;
    }
    let tagged = pairs.iter().any(|pair| {
        let tag = &landing.audio[pair.local].tag;
        !tag.artist.trim().is_empty()
            || tag
                .album_artist
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty())
    });
    if tagged {
        return Some(ArtistProof::Tags);
    }
    if pairs.iter().all(|pair| pair.support == Support::EmbeddedId) {
        return Some(ArtistProof::Ids);
    }
    if path_names_artist(target, found, landing) {
        return Some(ArtistProof::Path);
    }
    let close = pairs
        .iter()
        .filter(|pair| pair.distance <= CLOSE_PAIR)
        .count();
    if landing.audio.len() >= CLOSE_FILES
        && close >= CLOSE_FILES
        && close as f64 >= CLOSE_SHARE * found.release.tracks.len() as f64
    {
        return Some(ArtistProof::Tracklist);
    }
    let heard = heard?;
    let (answered, agreed) = pairs.iter().fold((0, 0), |(answered, agreed), pair| {
        match heard.get(&pair.local.to_string()) {
            Some(recordings) if !recordings.is_empty() => {
                let wanted = &found.release.tracks[pair.track].recording_id;
                let same = recordings
                    .iter()
                    .any(|recording| recording.eq_ignore_ascii_case(wanted));
                (answered + 1, agreed + usize::from(same))
            }
            _ => (answered, agreed),
        }
    });
    (agreed > 0 && agreed * 2 >= answered).then_some(ArtistProof::Audio)
}

/// Whether at least half the paired files' paths name the requested
/// artist (or the album, for a various-artists release). A file's path is read
/// as text only, never from disk: its folders below the client's downloads
/// folder, the folders of the remote name the client reported for it (a
/// Soulseek share's `Music\Artist\Album`, of which only `Album` is kept on
/// disk), and its file name without the paired track's title words, so a
/// self-titled song does not name itself.
fn path_names_artist(target: &Target, found: &FoundRelease, landing: &Landing) -> bool {
    let release = &found.release;
    let various = is_various(&target.artist_name) || is_various(&release.artist_text());
    let names: Vec<String> = if various {
        vec![target.album_title.clone(), release.title.clone()]
    } else {
        vec![target.artist_name.clone(), release.artist_text()]
    };
    let pairs = &found.scored.pairs;
    let named = pairs
        .iter()
        .filter(|pair| {
            let file = &landing.audio[pair.local];
            let mut folders: Vec<Vec<String>> = file
                .folders_below(&landing.downloads_folders)
                .iter()
                .chain(&remote_folders(&landing.remote_names, &file.file_name()))
                .flat_map(|part| segments(part))
                .collect();
            folders.dedup();
            let title: HashSet<String> = folded_words(&release.tracks[pair.track].title)
                .into_iter()
                .collect();
            let stem: Vec<Vec<String>> = file
                .path
                .file_stem()
                .map(|stem| segments(&stem.to_string_lossy()))
                .unwrap_or_default()
                .into_iter()
                .map(|segment| {
                    segment
                        .into_iter()
                        .filter(|word| !title.contains(word))
                        .collect::<Vec<_>>()
                })
                .filter(|segment| !segment.is_empty())
                .collect();
            names
                .iter()
                .any(|name| segments_name(&folders, &stem, name))
        })
        .count();
    named > 0 && named * 2 >= pairs.len()
}

/// The folders of each reported remote name whose file name is `file`
/// (compared folded, as the client may normalise it), without the
/// Soulseek share root (`@@music`). A name that is a path on this server
/// is left out: its folders below the downloads folder are read already,
/// and nothing above it is.
fn remote_folders(remote_names: &[String], file: &str) -> Vec<String> {
    let file = fold(file);
    remote_names
        .iter()
        .filter(|name| !std::path::Path::new(name.as_str()).has_root())
        .flat_map(|name| {
            let parts: Vec<&str> = name
                .split(['/', '\\'])
                .filter(|part| !part.is_empty())
                .collect();
            match parts.split_last() {
                Some((last, folders)) if fold(last) == file => folders
                    .iter()
                    .filter(|part| !part.starts_with("@@"))
                    .map(|part| (*part).to_owned())
                    .collect(),
                _ => Vec::new(),
            }
        })
        .collect()
}

/// Lowercase, folded words.
fn folded_words(text: &str) -> Vec<String> {
    words(text)
        .iter()
        .map(|word| fold(word))
        .filter(|word| !word.is_empty())
        .collect()
}

/// The pieces of one folder or file name that can each be a name, as
/// folded words: split at brackets and at " - " (and, separately, at every
/// hyphen and dot, for `Artist-Album-WEB-FLAC` scene names). A piece led
/// by a number ("01 Title", "1994 Album") is also kept without it.
fn segments(part: &str) -> Vec<Vec<String>> {
    let text = part
        .replace('_', " ")
        .replace(['\u{2013}', '\u{2014}'], "-");
    let mut out: Vec<Vec<String>> = Vec::new();
    for chunk in text.split(['[', ']', '(', ')', '{', '}']) {
        for piece in chunk.split(" - ").chain(chunk.split(['-', '.'])) {
            let words = folded_words(piece);
            let unnumbered: Vec<String> = words
                .iter()
                .skip_while(|word| word.chars().all(|ch| ch.is_ascii_digit()))
                .cloned()
                .collect();
            for segment in [words, unnumbered] {
                if !segment.is_empty() && !out.contains(&segment) {
                    out.push(segment);
                }
            }
        }
    }
    out
}

/// Words that never identify an artist on their own.
const NAME_STOP_WORDS: &[&str] = &["the", "a", "an", "and", "of", "feat", "ft"];

/// Whether one piece of the folder names (or, unless the name is only a
/// number, of the file name) is `name`. A name of up to two distinctive
/// words must be a whole piece, or its start ("Daft Punk feat. Pharrell"),
/// with or without a leading "The"; "Punk Goes Pop" is not Daft Punk and
/// "Air Supply" is not Air. A longer name needs most of its distinctive
/// words in one piece ("Tom Petty" for Tom Petty and the Heartbreakers).
fn segments_name(folders: &[Vec<String>], stem: &[Vec<String>], name: &str) -> bool {
    let all = folded_words(name);
    if all.is_empty() {
        return false;
    }
    let number = all
        .iter()
        .all(|word| word.chars().all(|ch| ch.is_ascii_digit()));
    let pieces: Vec<&Vec<String>> = if number {
        folders.iter().collect()
    } else {
        folders.iter().chain(stem).collect()
    };
    let distinctive: Vec<&String> = all
        .iter()
        .filter(|word| word.chars().count() >= 2 && !NAME_STOP_WORDS.contains(&word.as_str()))
        .collect();
    if distinctive.len() >= 3 {
        return pieces.iter().any(|piece| {
            let present = distinctive
                .iter()
                .filter(|word| piece.contains(word))
                .count();
            present * 2 > distinctive.len()
        });
    }
    let mut phrases = vec![all.as_slice()];
    if all.len() > 1 && ["the", "a", "an"].contains(&all[0].as_str()) {
        phrases.push(&all[1..]);
    }
    pieces.iter().any(|piece| {
        phrases.iter().any(|phrase| {
            piece.as_slice() == *phrase || (phrase.len() >= 2 && piece.starts_with(phrase))
        })
    })
}

/// Untagged files import only with evidence of their artist
/// ([`artist_proof`]): without it a file named "01 - The Arrival.flac" by
/// anyone would pass for the requested single.
struct ArtistEvidence;

impl ImportSpec for ArtistEvidence {
    fn name(&self) -> &'static str {
        "artist_evidence"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        if found.scored.pairs.is_empty() {
            return Verdict::accept();
        }
        match artist_proof(subject.target, subject.landing, found, subject.heard) {
            Some(ArtistProof::Tags) => Verdict::accept(),
            Some(proof) => Verdict::note(proof.note().to_owned()),
            None => Verdict::hold(
                Reason::NoArtistEvidence,
                format!(
                    "no artist tags, no '{}' in the folder names, no AcoustID match, \
                     and too few tracks fit closely",
                    subject.target.artist_name
                ),
            ),
        }
    }
}

/// A file whose own artist tag names someone
/// other than the release's artist, while its title does not name the
/// track or its length does not fit, is another song. Held on its own.
/// An artist conflict alone holds nothing: classical files often credit
/// the composer where the release credits the performer.
struct OwnArtist;

impl ImportSpec for OwnArtist {
    fn name(&self) -> &'static str {
        "own_artist"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        let conflicting = artist_conflicts(subject.target, subject.landing, found).len();
        if conflicting == 0 {
            return Verdict::accept();
        }
        if conflicting == found.scored.pairs.len() {
            return Verdict::hold(
                Reason::TagMismatch,
                "every file's artist tag names someone else",
            );
        }
        Verdict::note(format!(
            "{conflicting} file(s) held: their artist tag names someone else"
        ))
    }
}

/// The paired files [`artist_conflict`] holds, by landed file index.
pub fn artist_conflicts(
    target: &Target,
    landing: &Landing,
    found: &FoundRelease,
) -> HashSet<usize> {
    found
        .scored
        .pairs
        .iter()
        .filter(|pair| artist_conflict(target, landing, found, pair).is_some())
        .map(|pair| pair.local)
        .collect()
}

/// Why the file of `pair` is another song by someone else: its artist tag
/// shares no words with the requested artist, the release's artist or the
/// track's own credit (one side's words must hold the other's, so
/// "Beethoven" agrees with "Ludwig van Beethoven"), and its title tag is
/// further than the name gate from the track's or its length is off by
/// more than `max(15 s, 10%)`. Untagged fields never conflict, and a file
/// that pairs by its own MusicBrainz ids is never judged by its text.
pub fn artist_conflict(
    target: &Target,
    landing: &Landing,
    found: &FoundRelease,
    pair: &TrackPair,
) -> Option<String> {
    if pair.support == Support::EmbeddedId {
        return None;
    }
    let file = landing.audio.get(pair.local)?;
    let release = &found.release;
    let track = release.tracks.get(pair.track)?;
    let tagged = file.tag.artist.trim();
    if tagged.is_empty() {
        return None;
    }
    let credits = [
        target.artist_name.clone(),
        release.artist_text(),
        credit_text(&track.artists),
    ];
    let known: Vec<&String> = credits
        .iter()
        .filter(|name| !name.trim().is_empty() && !is_various(name))
        .collect();
    let agrees = known.iter().any(|name| {
        string_dist(tagged, name) <= NAME_GATE
            || artist_subset(tagged, name)
            || words_within(tagged, name)
    });
    if known.is_empty() || agrees {
        return None;
    }
    let title = file.tag.title.trim();
    let title_weak = !title.is_empty()
        && string_dist(
            &strip_edition_words(title),
            &strip_edition_words(&track.title),
        ) > NAME_GATE;
    let length_off = match (file.header.duration_seconds, track.length_ms) {
        (Some(seconds), Some(ms)) => {
            let expected = ms as f64 / 1000.0;
            (seconds - expected).abs() > (0.10 * expected).max(15.0)
        }
        _ => false,
    };
    (title_weak || length_off)
        .then(|| format!("the file's artist tag names {tagged}, not {}", known[0]))
}

/// Whether one name's distinctive words all appear in the other's.
fn words_within(left: &str, right: &str) -> bool {
    let distinctive = |name: &str| -> HashSet<String> {
        folded_words(name)
            .into_iter()
            .filter(|word| !NAME_STOP_WORDS.contains(&word.as_str()))
            .collect()
    };
    let (left, right) = (distinctive(left), distinctive(right));
    !left.is_empty() && !right.is_empty() && (left.is_subset(&right) || right.is_subset(&left))
}

/// Lidarr's `CloseTrackMatch`, per file: a file whose pair is too far
/// (its tags name another song) is held on its own and the rest
/// import.
struct CloseTrackMatch;

impl ImportSpec for CloseTrackMatch {
    fn name(&self) -> &'static str {
        "close_track_match"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        let far = found
            .scored
            .pairs
            .iter()
            .filter(|pair| pair.distance > ACCEPT_TRACK)
            .count();
        if far == 0 {
            return Verdict::accept();
        }
        if far == found.scored.pairs.len() {
            return Verdict::hold(Reason::TagMismatch, "no file is close to its track");
        }
        Verdict::note(format!("{far} file(s) held: their tags name other tracks"))
    }
}

/// Files the release does not account for (bonus tracks MusicBrainz
/// lacks, scene extras) are left out; a download that is
/// mostly such files is not this release.
struct FilesAccountedFor;

impl ImportSpec for FilesAccountedFor {
    fn name(&self) -> &'static str {
        "files_accounted_for"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        let extra = found.scored.unmatched.len();
        if extra == 0 {
            return Verdict::accept();
        }
        if extra > found.scored.pairs.len() {
            return Verdict::hold(
                Reason::MostlyUnmatched,
                format!("{extra} of the files match no track of the release"),
            );
        }
        Verdict::note(format!("{extra} extra file(s) not imported"))
    }
}

/// A track download must have landed the requested recording, at the
/// requested length (fail over, never blocklist).
struct RightTrack;

impl ImportSpec for RightTrack {
    fn name(&self) -> &'static str {
        "right_track"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let target = subject.target;
        if !target.is_track {
            return Verdict::accept();
        }
        let Some(found) = subject.matched.and_then(MatchSummary::best) else {
            return Verdict::accept();
        };
        let wrong = |detail: String| {
            Verdict::reject(Reason::WrongTrack, Disposition::Permanent, None, detail)
        };
        let Some(pair) = found.requested_pair(target) else {
            return wrong("no landed file is the requested track".to_owned());
        };
        match wrong_track(found, pair, subject.landing) {
            Some(detail) => wrong(detail),
            None => Verdict::accept(),
        }
    }
}

/// Why the file of `pair` is not the requested track: its length is off
/// by more than `max(15 s, 10%)`.
fn wrong_track(found: &FoundRelease, pair: &TrackPair, landing: &Landing) -> Option<String> {
    let track = found.release.tracks.get(pair.track)?;
    let file = landing.audio.get(pair.local)?;
    let (Some(length_ms), Some(seconds)) = (track.length_ms, file.header.duration_seconds) else {
        return None;
    };
    let expected = length_ms as f64 / 1000.0;
    ((seconds - expected).abs() > (0.10 * expected).max(15.0))
        .then(|| format!("the file is {seconds:.0}s, the requested track {expected:.0}s"))
}

/// Tracks the library already holds are not imported twice.
struct NotAlreadyImported;

impl ImportSpec for NotAlreadyImported {
    fn name(&self) -> &'static str {
        "not_already_imported"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        let Some(matched) = subject.matched else {
            return Verdict::accept();
        };
        let Some(found) = matched.best() else {
            return Verdict::accept();
        };
        let owned = found
            .scored
            .pairs
            .iter()
            .filter(|pair| matched.owns(&found.release.tracks[pair.track]))
            .count();
        if owned == 0 {
            return Verdict::accept();
        }
        Verdict::note(format!("{owned} track(s) already in the library"))
    }
}

// ---------------------------------------------------------------------------
// Fingerprints.
// ---------------------------------------------------------------------------

/// How far a file's length may be off its track and still vouch for it
/// against a fingerprint that names another recording. Copies of one
/// master differ by a second or two (pregap, encoder padding, MusicBrainz
/// rounding to the second), while another recording of a song is rarely
/// within 3 s of it: a `max(15 s, 10%)` window lets most same-length pop songs
/// through whatever AcoustID heard.
pub const SAME_MASTER_SECS: f64 = 3.0;

/// For the ids our lookup returns: AcoustID
/// confidently heard recordings, none of them the expected one, and the
/// file's length is not within [`SAME_MASTER_SECS`] of the expected
/// track's. A length that agrees outranks a conflicting fingerprint, and
/// no answer never holds anything.
pub fn fingerprint_disagrees(
    heard: &[String],
    expected_recording: &str,
    file_seconds: Option<f64>,
    expected_seconds: Option<f64>,
) -> bool {
    if heard.is_empty() || expected_recording.is_empty() {
        return false;
    }
    if heard
        .iter()
        .any(|recording| recording.eq_ignore_ascii_case(expected_recording))
    {
        return false;
    }
    let length_agrees = match (file_seconds, expected_seconds) {
        (Some(file), Some(expected)) if expected > 0.0 => {
            (file - expected).abs() <= SAME_MASTER_SECS
        }
        _ => false,
    };
    !length_agrees
}

// ---------------------------------------------------------------------------
// Text helpers.
// ---------------------------------------------------------------------------

/// Lowercase words, splitting on anything that is not a letter or digit
/// (underscores and dots included, so `Live_EP` and `Box.Set` read right).
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `phrase` (one or more words) appears as consecutive words.
fn has_phrase(text: &[String], phrase: &str) -> bool {
    let wanted: Vec<&str> = phrase.split(' ').collect();
    text.windows(wanted.len())
        .any(|window| window.iter().zip(&wanted).all(|(have, want)| have == want))
}

/// The words that make an album title itself: edition suffixes, artist
/// words, numbers and noise removed, each word folded.
fn content_words(title: &str, artist_words: &[String]) -> Vec<String> {
    words(&strip_edition_words(title))
        .into_iter()
        .filter(|word| !artist_words.contains(word))
        .filter(|word| !NOISE_WORDS.contains(&word.as_str()))
        .filter(|word| !word.chars().all(|ch| ch.is_ascii_digit()))
        .map(|word| fold(&word))
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edition_phrases_match_whole_words() {
        let text = words("Led_Zeppelin-Live_EP Box.Set");
        assert!(has_phrase(&text, "live"));
        assert!(has_phrase(&text, "box set"));
        assert!(!has_phrase(&words("Alive"), "live"));
    }

    #[test]
    fn fingerprints_hold_only_a_confident_other_recording() {
        let other = vec!["b".to_owned()];
        assert!(fingerprint_disagrees(&other, "a", Some(200.0), Some(260.0)));
        assert!(fingerprint_disagrees(&other, "a", Some(255.0), Some(260.0)));
        assert!(!fingerprint_disagrees(
            &other,
            "a",
            Some(258.0),
            Some(260.0)
        ));
        assert!(!fingerprint_disagrees(&["A".to_owned()], "a", None, None));
        assert!(!fingerprint_disagrees(&[], "a", Some(1.0), Some(260.0)));
    }

    /// Whether a file at `folder/stem`, paired with a track titled
    /// `title`, is named as by `name`.
    fn named(name: &str, folder: &str, stem: &str, title: &str) -> bool {
        let title = folded_words(title);
        let stem: Vec<Vec<String>> = segments(stem)
            .into_iter()
            .map(|segment| segment.into_iter().filter(|w| !title.contains(w)).collect())
            .filter(|segment: &Vec<String>| !segment.is_empty())
            .collect();
        segments_name(&segments(folder), &stem, name)
    }

    #[test]
    fn a_path_names_the_artist_only_as_a_whole_piece() {
        let rows = [
            ("Daft Punk", "Punk Goes Pop Vol. 6", "05 - Get Lucky", false),
            (
                "Daft Punk",
                "Daft Punk - Discovery",
                "01 - One More Time",
                true,
            ),
            (
                "Daft Punk",
                "singles",
                "Daft Punk feat. Pharrell - Get Lucky",
                true,
            ),
            ("Pink Floyd", "Pink - Funhouse", "01 - So What", false),
            ("La Roux", "La Boheme", "01 - Aria", false),
            ("Air", "Air Supply - Greatest", "01 - Lost in Love", false),
            (
                "Air",
                "Air - Moon Safari (1998) [FLAC]",
                "01 - La Femme",
                true,
            ),
            ("Yes", "1971 Yes - Fragile", "01 - Roundabout", true),
            (
                "Tom Petty and the Heartbreakers",
                "Tom Petty - Damn the Torpedoes",
                "01 - Refugee",
                true,
            ),
            ("311", "311 - Grassroots", "01 - Lucky", true),
            ("311", "rips", "311 Lucky", false),
            (
                "Blink-182",
                "blink-182 - Enema of the State",
                "01 - Dumpweed",
                true,
            ),
            ("Sum 41", "Sum_41_-_Chuck", "01 - No Reason", true),
            (
                "The The",
                "The The - Soul Mining",
                "01 - I've Been Waitin'",
                true,
            ),
            (
                "Portishead",
                "Portishead-Dummy-WEB-FLAC-1994",
                "01-mysterons",
                true,
            ),
            ("Black Sabbath", "rips", "01 - Black Sabbath", false),
            (
                "Yan Qing",
                "Dan Romer - Original Score",
                "01 - The Arrival",
                false,
            ),
        ];
        for (name, folder, stem, expected) in rows {
            let title = stem.split(" - ").last().unwrap_or(stem);
            assert_eq!(
                named(name, folder, stem, title),
                expected,
                "{name} in {folder}/{stem}"
            );
        }
    }

    #[test]
    fn remote_folders_skip_the_share_root_and_local_paths() {
        let names = vec![
            r"@@x\Music\Portishead\Singles\01 - Glory Box.flac".to_owned(),
            "/srv/Portishead/downloads/Singles/01 - Glory Box.flac".to_owned(),
        ];
        assert_eq!(
            remote_folders(&names, "01 - Glory Box.flac"),
            vec!["Music", "Portishead", "Singles"]
        );
        assert!(remote_folders(&names, "02 - Roads.flac").is_empty());
    }

    #[test]
    fn an_artist_tag_agrees_word_for_word() {
        assert!(words_within("Beethoven", "Ludwig van Beethoven"));
        assert!(!words_within("Dan Romer", "Yan Qing"));
    }

    #[test]
    fn content_words_ignore_editions_and_noise() {
        let artist = words("Led Zeppelin");
        assert!(content_words("Led Zeppelin (Remastered) [Disc 1]", &artist).is_empty());
        assert_eq!(content_words("Led Zeppelin II", &artist), vec!["ii"]);
    }
}
