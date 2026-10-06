//! Import specifications: the checks a landed download must pass.
//!
//! Lidarr's import decision engine is a list of specifications, each
//! answering accept or a reason; v2 ported the same idea as
//! `acquisition/specs` with typed reject codes and a disposition that
//! decides what a rejection means for failover. This is that list for the
//! import side, as pure functions over a [`Subject`]:
//!
//! - file checks run on what the probe read, before any lookup, cheapest
//!   and most decisive first: files present and whole on disk, audio
//!   present, a walk that finished, samples, wrong edition, wrong album,
//!   quality range, upgrade floor;
//! - match checks run on the closest release the matching engine found:
//!   a release found, ids that agree, a close album match, files the
//!   release accounts for, the right track for a track download.
//!
//! Every check is recorded (accept, hold, or reject with its reason), so
//! the views can say why a download was held or failed over.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use super::matching::{MatchState, MatchSummary};
use super::probe::Landing;
use crate::acquire::downloads::quarantine::QuarantineReason;
use crate::library::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK};
use crate::library::matching::strings::{fold, strip_edition_words};

/// What a rejection means for retry and failover (v2 `Disposition`).
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
    pub code: &'static str,
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
    Hold { code: &'static str, detail: String },
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

    fn hold(code: &'static str, detail: impl Into<String>) -> Self {
        Verdict::Hold {
            code,
            detail: detail.into(),
        }
    }

    fn reject(
        code: &'static str,
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
    /// `user`, `retry`, or `upgrade`.
    pub origin: String,
    /// The last-resort re-pull: hold a wrong track instead of failing over.
    pub hold_on_wrong_track: bool,
    /// Byte sizes the client advertised, by the exact reported path.
    pub expected_sizes: HashMap<PathBuf, u64>,
    /// Files may still appear: a missing file is worth another pass.
    pub wait_for_files: bool,
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

/// The client said files landed; something must be there (v2
/// `SOURCE_FILE_MISSING`, a local fault: the peer delivered).
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
                "files_missing",
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
                    "files_missing",
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

/// Each file holds the bytes the client advertised (v2 `SIZE_MISMATCH`):
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
                "size_mismatch",
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
                "too_many_files",
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
                    "corrupt",
                    Disposition::Permanent,
                    Some(QuarantineReason::Corrupt),
                    "no audio file could be read",
                );
            }
            if landing.nothing_found() {
                return Verdict::accept();
            }
            return Verdict::reject(
                "no_audio",
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
                "sample",
                Disposition::Permanent,
                Some(QuarantineReason::VerifyFailed),
                "the download is a sample, not the release",
            );
        }
        Verdict::note(format!("{samples} sample file(s) left out"))
    }
}

/// Product markers that mean a different release than a studio album
/// (v2 `wrong_edition`). Remasters and deluxe editions are the same album.
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
                "wrong_edition",
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

/// Most tagged files name another album by the same artist (v2
/// `_folder_names_wrong_album`): their album titles carry words the
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
                "wrong_album",
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

/// The files' real quality must sit inside the policy band (v2
/// `post_download_quality_mismatch`). Never blocklisted: a peer's labels
/// were wrong, not its files broken.
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
        if super::quality::in_range(tier, &policy.quality_min, &policy.quality_max) {
            return Verdict::accept();
        }
        Verdict::reject(
            "quality_rejected",
            Disposition::Permanent,
            None,
            format!(
                "files are {tier}, outside {} to {}",
                policy.quality_min, policy.quality_max
            ),
        )
    }
}

/// An upgrade must strictly beat what the library holds (v2
/// `upgrade_floor`); equal or worse is never an upgrade.
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
            "not_an_upgrade",
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
                "no_release",
                "MusicBrainz has no release of the requested album to check the files against",
            ),
            Some(MatchState::Unavailable(detail)) => Verdict::reject(
                "musicbrainz_unavailable",
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
                "not_pinned_edition",
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

/// The files' own MusicBrainz ids must not contradict every candidate
/// (v2 `tag_mismatch` on a recording MBID conflict).
struct IdsAgree;

impl ImportSpec for IdsAgree {
    fn name(&self) -> &'static str {
        "ids_agree"
    }

    fn check(&self, subject: &Subject<'_>) -> Verdict {
        match subject.matched.map(|matched| &matched.state) {
            Some(MatchState::Contradictory) => Verdict::hold(
                "tag_mismatch",
                "the files' MusicBrainz ids name a different release",
            ),
            _ => Verdict::accept(),
        }
    }
}

/// Lidarr's `CloseAlbumMatch`: the album as a whole must be close, and
/// its title and artist must agree with the release.
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
            return Verdict::hold("weak_match", "no file pairs with a track of the release");
        }
        let distance = found.scored.library_distance();
        if !found.scored.names_agree {
            return Verdict::hold(
                "weak_match",
                "the album title or artist does not agree with the release",
            );
        }
        if distance > ACCEPT_ALBUM {
            return Verdict::hold(
                "weak_match",
                format!("album distance {distance:.2} is above {ACCEPT_ALBUM:.2}"),
            );
        }
        Verdict::accept()
    }
}

/// Lidarr's `CloseTrackMatch`, per file: a file whose pair is too far
/// (its tags name another song) is held on its own, as v2 held a
/// tag-mismatched file and imported the rest.
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
            return Verdict::hold("tag_mismatch", "no file is close to its track");
        }
        Verdict::note(format!("{far} file(s) held: their tags name other tracks"))
    }
}

/// Files the release does not account for (bonus tracks MusicBrainz
/// lacks, scene extras) are left out, as v2 did; a download that is
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
                "mostly_unmatched",
                format!("{extra} of the files match no track of the release"),
            );
        }
        Verdict::note(format!("{extra} extra file(s) not imported"))
    }
}

/// A track download must have landed the requested recording, at the
/// requested length (v2 `WRONG_TRACK`: fail over, never blocklist).
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
            if target.hold_on_wrong_track {
                Verdict::hold("wrong_track", detail)
            } else {
                Verdict::reject("wrong_track", Disposition::Permanent, None, detail)
            }
        };
        let Some(pair) = found.requested_pair(target) else {
            return wrong("no landed file is the requested track".to_owned());
        };
        let track = &found.release.tracks[pair.track];
        let file = &subject.landing.audio[pair.local];
        if let (Some(length_ms), Some(seconds)) = (track.length_ms, file.header.duration_seconds) {
            let expected = length_ms as f64 / 1000.0;
            if (seconds - expected).abs() > (0.10 * expected).max(15.0) {
                return wrong(format!(
                    "the file is {seconds:.0}s, the requested track {expected:.0}s"
                ));
            }
        }
        Verdict::accept()
    }
}

/// Tracks the library already holds are not imported twice (v2's
/// position dedup).
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

/// v2's `_fingerprint_disagrees`, for the ids our lookup returns: AcoustID
/// confidently heard recordings, none of them the expected one, and the
/// file's length does not vouch for the expected track (within
/// `max(15 s, 10%)`). A length that agrees outranks a conflicting
/// fingerprint, and no answer never holds anything.
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
            (file - expected).abs() <= (0.10 * expected).max(15.0)
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
        assert!(!fingerprint_disagrees(
            &other,
            "a",
            Some(255.0),
            Some(260.0)
        ));
        assert!(!fingerprint_disagrees(&["A".to_owned()], "a", None, None));
        assert!(!fingerprint_disagrees(&[], "a", Some(1.0), Some(260.0)));
    }

    #[test]
    fn content_words_ignore_editions_and_noise() {
        let artist = words("Led Zeppelin");
        assert!(content_words("Led Zeppelin (Remastered) [Disc 1]", &artist).is_empty());
        assert_eq!(content_words("Led Zeppelin II", &artist), vec!["ii"]);
    }
}
