//! What a manual search found, as the person sees it and as the worker
//! fetches it.
//!
//! Each source ranks its own results the way the automatic path does
//! (Soulseek folders against the edition's tracklist, Usenet releases by
//! title and file count, plugin releases by the plugin's score) and turns
//! them into [`Candidate`]s. A candidate carries a view for the page and a
//! [`Pin`] that tells the source exactly what to fetch once it is picked.
//! Usenet and plugin pins hold the release identity, not the NZB URL, so
//! no indexer key is ever written to the database.
//!
//! A candidate outside the download settings (quality band, size cap,
//! terms, minimum score) is still listed, with every reason it is outside
//! them and tier [`CandidateTier::Refused`], after every candidate that is
//! not. Automatic picks never take one; a person may, after confirming.
//! What cannot be fetched at all (blocklisted, password-protected, past
//! retention, nothing importable) is never listed.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::downloads::source_tag::SourceTag;
use crate::plugin_source::{Band, ScoredRelease, release_identity};
use crate::slskd::SearchResult;
use crate::slskd::folders::FolderPick;
use crate::target::SearchTarget;
use crate::target::releases::release_fit;
use crate::usenet::newznab::{IndexerResult, usenet_identity};

/// Candidates kept per source, at most.
pub const MAX_PER_SOURCE: usize = 25;

/// How strongly a candidate is recommended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CandidateTier {
    /// Looks like the whole edition, in a quality the settings accept.
    Recommended,
    /// Usable, with the caveat its note gives.
    Possible,
    /// Outside the download settings: picked only after a confirm.
    Refused,
}

/// Why a candidate is outside the download settings. Declared in the order
/// the page names them: the first that applies is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRefusal {
    /// Its quality is outside the band the landing keeps.
    QualityOutsideRange,
    /// It is bigger than the size cap.
    OverSizeCap,
    /// Its title holds an ignored term.
    IgnoredTerm,
    /// Its title lacks a required term.
    RequiredTermMissing,
    /// The source scores it below the manual minimum.
    BelowMinimumScore,
}

impl CandidateRefusal {
    /// The reason, as the end of a sentence.
    pub fn text(self) -> &'static str {
        match self {
            Self::QualityOutsideRange => "its quality is outside your range",
            Self::OverSizeCap => "it is over your size limit",
            Self::IgnoredTerm => "its title has a term you ignore",
            Self::RequiredTermMissing => "its title lacks a term you require",
            Self::BelowMinimumScore => "the source scores it below your minimum",
        }
    }
}

/// The stable code of a [`CandidateNote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CandidateNoteCode {
    /// The folder holds the whole edition.
    Complete,
    /// The folder misses some of the edition's tracks.
    Incomplete,
    /// Some track lengths are off the edition's.
    Lengths,
    /// The search does not show the quality of some files.
    Quality,
    /// The edition's tracklist could not be read.
    NoTracklist,
    /// The release title names the album and artist.
    NamesAlbum,
    /// The release title does not clearly name the album.
    TitleUnclear,
    /// The source rates it a strong match.
    Strong,
    /// The source rates it a weak match.
    Weak,
}

/// Why a candidate sits where it does: one stable code and one sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CandidateNote {
    /// Stable code.
    pub code: CandidateNoteCode,
    /// What the candidate looks like, in one sentence.
    pub text: String,
}

impl CandidateNote {
    fn new(code: CandidateNoteCode, text: String) -> Self {
        Self { code, text }
    }
}

/// One file a candidate would fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CandidateFile {
    /// File path as the source names it.
    pub filename: String,
    /// Size in bytes, 0 when unknown.
    pub size: i64,
    /// Lowercase extension.
    pub extension: String,
    /// Bitrate in kbps for lossy files.
    pub bitrate: Option<i32>,
    /// Advertised length in seconds.
    pub duration_seconds: Option<f64>,
}

/// One candidate as the search page shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchCandidateView {
    /// Position in the job's list; send it back to pick this one.
    pub candidate_index: usize,
    /// `soulseek`, `usenet` or `plugin:<name>`.
    pub source: String,
    /// Folder name or release title.
    pub title: String,
    /// How strongly it is recommended.
    pub tier: CandidateTier,
    /// Why, in one sentence.
    pub note: CandidateNote,
    /// Every way it is outside the download settings, the one to show
    /// first. Empty when it is inside them.
    #[serde(default)]
    pub refusal: Vec<CandidateRefusal>,
    /// Soulseek peer or plugin account, when there is one.
    pub username: Option<String>,
    /// Files it would fetch, when the source lists them.
    pub files: Vec<CandidateFile>,
    /// Files in the candidate (0 when the source does not say).
    pub file_count: usize,
    /// Total size in bytes (0 when unknown).
    pub size_bytes: u64,
    /// Edition tracks it holds (Soulseek, when the tracklist is known).
    pub tracks_matched: Option<usize>,
    /// Soulseek: held tracks whose length is unknown or off the edition's,
    /// when the tracklist is known.
    pub length_misses: Option<usize>,
    /// Tracks on the edition, when known.
    pub tracks_total: Option<usize>,
    /// Format label (`FLAC 24-bit`, `MP3 320`, `lossless`), when known.
    pub format: Option<String>,
    /// Soulseek: the peer has a free upload slot.
    pub has_free_slot: Option<bool>,
    /// Soulseek: files waiting in the peer's upload queue.
    pub queue_length: Option<i64>,
    /// Soulseek: the peer's upload speed in bytes per second.
    pub upload_speed: Option<i64>,
    /// Usenet: the indexer that listed it.
    pub indexer: Option<String>,
    /// Usenet: when it was posted (unix seconds).
    pub posted_at: Option<f64>,
    /// Usenet: how often it was grabbed, when the indexer says.
    pub grabs: Option<i64>,
    /// Plugin: the plugin's own score for it, from 0 to 1.
    pub score: Option<f64>,
}

/// One file of a pinned Soulseek folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    /// Path on the peer.
    pub filename: String,
    /// Advertised size.
    pub size: i64,
}

/// What the source fetches once a candidate is picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pin {
    /// These files from this peer.
    Soulseek {
        /// Peer.
        username: String,
        /// Folder on the peer.
        folder: String,
        /// Files to enqueue.
        files: Vec<PinnedFile>,
    },
    /// The release with this identity, found again at enqueue time.
    Release {
        /// Usenet: normalised title plus size; plugins: their identity.
        identity: String,
    },
}

/// One stored candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// What the page shows.
    pub view: SearchCandidateView,
    /// What the worker fetches.
    pub pin: Pin,
}

impl Candidate {
    /// Outside the download settings: automatic picks never take it.
    pub fn is_refused(&self) -> bool {
        !self.view.refusal.is_empty()
    }

    /// The reason the page names first, when it is refused.
    pub fn first_refusal(&self) -> Option<CandidateRefusal> {
        self.view.refusal.first().copied()
    }

    /// Whether a confirmed pick of it keeps files outside the quality band.
    pub fn waives_quality(&self) -> bool {
        self.view
            .refusal
            .contains(&CandidateRefusal::QualityOutsideRange)
    }

    /// Record why it is refused; any reason makes its tier `refused`.
    fn refused(mut self, mut reasons: Vec<CandidateRefusal>) -> Self {
        reasons.sort();
        reasons.dedup();
        if !reasons.is_empty() {
            self.view.tier = CandidateTier::Refused;
        }
        self.view.refusal = reasons;
        self
    }

    /// The identity the wanted watcher remembers this candidate by.
    pub fn seen_identity(&self) -> String {
        match &self.pin {
            Pin::Soulseek {
                username, folder, ..
            } => format!("{username}/{folder}"),
            Pin::Release { identity } => identity.clone(),
        }
    }

    /// A ranked Soulseek folder; one whose known quality is outside the
    /// band is refused.
    pub fn from_folder(pick: FolderPick) -> Self {
        let refusal = if pick.out_of_band {
            vec![CandidateRefusal::QualityOutsideRange]
        } else {
            Vec::new()
        };
        let has_free_slot = pick.files.iter().any(|file| file.has_free_slot);
        let queue_length = pick.files.iter().map(|file| file.queue_length).min();
        let upload_speed = pick.files.iter().map(|file| file.upload_speed).max();
        let (tier, note) = folder_note(&pick);
        let title = pick
            .folder
            .rsplit(['\\', '/'])
            .find(|part| !part.is_empty())
            .unwrap_or(&pick.folder)
            .to_owned();
        let size_bytes = pick
            .files
            .iter()
            .map(|file| u64::try_from(file.size.max(0)).unwrap_or(0))
            .sum();
        let format = soulseek_format(&pick.files);
        let files: Vec<CandidateFile> = pick
            .files
            .iter()
            .map(|file| CandidateFile {
                filename: file.filename.clone(),
                size: file.size,
                extension: file.extension.clone(),
                bitrate: file.bitrate,
                duration_seconds: file.duration,
            })
            .collect();
        let pin = Pin::Soulseek {
            username: pick.username.clone(),
            folder: pick.folder.clone(),
            files: pick
                .files
                .iter()
                .map(|file| PinnedFile {
                    filename: file.filename.clone(),
                    size: file.size,
                })
                .collect(),
        };
        let known = pick.total > 0;
        Self {
            view: SearchCandidateView {
                candidate_index: 0,
                source: SourceTag::Soulseek.to_string(),
                title,
                tier,
                note,
                refusal: Vec::new(),
                username: Some(pick.username),
                file_count: files.len(),
                files,
                size_bytes,
                tracks_matched: known.then_some(pick.coverage),
                length_misses: known.then_some(pick.duration_misses),
                tracks_total: known.then_some(pick.total),
                format,
                has_free_slot: Some(has_free_slot),
                queue_length,
                upload_speed,
                indexer: None,
                posted_at: None,
                grabs: None,
                score: None,
            },
            pin,
        }
        .refused(refusal)
    }

    /// A Usenet release, judged on its title and file count, outside the
    /// download settings for each reason in `refusal`.
    pub fn from_release(
        hit: &IndexerResult,
        target: &SearchTarget,
        refusal: Vec<CandidateRefusal>,
    ) -> Self {
        let fit = release_fit(hit, target);
        let (tier, note) = match fit.names {
            0 => (
                CandidateTier::Recommended,
                CandidateNote::new(
                    CandidateNoteCode::NamesAlbum,
                    "The release title names this album and artist.".to_owned(),
                ),
            ),
            1 => (
                CandidateTier::Possible,
                CandidateNote::new(
                    CandidateNoteCode::TitleUnclear,
                    "The release title names this album but not the artist.".to_owned(),
                ),
            ),
            _ => (
                CandidateTier::Possible,
                CandidateNote::new(
                    CandidateNoteCode::TitleUnclear,
                    "The release title does not clearly name this album.".to_owned(),
                ),
            ),
        };
        let release = &hit.usenet;
        Self {
            view: SearchCandidateView {
                candidate_index: 0,
                source: SourceTag::Usenet.to_string(),
                title: release.title.clone(),
                tier,
                note,
                refusal: Vec::new(),
                username: None,
                files: Vec::new(),
                file_count: release
                    .files
                    .and_then(|files| usize::try_from(files).ok())
                    .unwrap_or(0),
                size_bytes: release.size_bytes,
                tracks_matched: None,
                length_misses: None,
                tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
                format: usenet_format(&release.category_ids, &release.title),
                has_free_slot: None,
                queue_length: None,
                upload_speed: None,
                indexer: Some(release.indexer_name.clone()),
                posted_at: release.usenet_date,
                grabs: release.grabs,
                score: None,
            },
            pin: Pin::Release {
                identity: usenet_identity(&release.title, release.size_bytes),
            },
        }
        .refused(refusal)
    }

    /// A plugin release, judged by the plugin's own score.
    pub fn from_plugin(source: &SourceTag, scored: ScoredRelease) -> Self {
        let (tier, note) = if scored.band == Band::Auto {
            (
                CandidateTier::Recommended,
                CandidateNote::new(
                    CandidateNoteCode::Strong,
                    "The source rates this a strong match.".to_owned(),
                ),
            )
        } else {
            (
                CandidateTier::Possible,
                CandidateNote::new(
                    CandidateNoteCode::Weak,
                    "The source rates this a weak match. Check the title before picking."
                        .to_owned(),
                ),
            )
        };
        let ScoredRelease {
            release,
            score,
            refusal,
            ..
        } = scored;
        let username = release
            .files
            .first()
            .map(|file| file.username.clone())
            .filter(|name| !name.is_empty());
        let files: Vec<CandidateFile> = release
            .files
            .iter()
            .map(|file| CandidateFile {
                filename: file.filename.clone(),
                size: file.size,
                extension: extension_of(&file.filename),
                bitrate: None,
                duration_seconds: None,
            })
            .collect();
        Self {
            pin: Pin::Release {
                identity: release_identity(&release),
            },
            view: SearchCandidateView {
                candidate_index: 0,
                source: source.to_string(),
                title: release.title.clone(),
                tier,
                note,
                refusal: Vec::new(),
                username,
                file_count: files.len(),
                files,
                size_bytes: u64::try_from(release.size_bytes.max(0)).unwrap_or(0),
                tracks_matched: None,
                length_misses: None,
                tracks_total: None,
                format: (!release.quality_tier.is_empty()).then(|| release.quality_tier.clone()),
                has_free_slot: None,
                queue_length: None,
                upload_speed: None,
                indexer: None,
                posted_at: release.usenet_date,
                grabs: None,
                score: Some(score),
            },
        }
        .refused(refusal)
    }
}

/// Tier and note for one folder: the first thing wrong with it, or that
/// it looks complete.
fn folder_note(pick: &FolderPick) -> (CandidateTier, CandidateNote) {
    let possible = |code: CandidateNoteCode, text: String| {
        (CandidateTier::Possible, CandidateNote::new(code, text))
    };
    if pick.total == 0 {
        return possible(
            CandidateNoteCode::NoTracklist,
            format!(
                "{} audio files. The edition's tracklist could not be read to compare them.",
                pick.coverage
            ),
        );
    }
    if pick.coverage < pick.total {
        return possible(
            CandidateNoteCode::Incomplete,
            format!(
                "Holds {} of the {} tracks on this edition.",
                pick.coverage, pick.total
            ),
        );
    }
    if pick.duration_misses > 0 {
        let text = if pick.duration_misses == 1 {
            "1 track length does not match this edition.".to_owned()
        } else {
            format!(
                "{} track lengths do not match this edition.",
                pick.duration_misses
            )
        };
        return possible(CandidateNoteCode::Lengths, text);
    }
    if pick.unknown_quality {
        return possible(
            CandidateNoteCode::Quality,
            "The search does not show the quality of some files.".to_owned(),
        );
    }
    (
        CandidateTier::Recommended,
        CandidateNote::new(
            CandidateNoteCode::Complete,
            format!("Holds all {} tracks of this edition.", pick.total),
        ),
    )
}

/// `FLAC 24-bit`, `MP3 320` and so on, from the folder's files: the lowest
/// bitrate of lossy files, the highest bit depth of lossless ones.
fn soulseek_format(files: &[SearchResult]) -> Option<String> {
    let first = files.first()?;
    let extension = first.extension.to_ascii_uppercase();
    if extension.is_empty() {
        return None;
    }
    let lossless = matches!(
        extension.as_str(),
        "FLAC" | "ALAC" | "WAV" | "APE" | "WV" | "AIFF" | "AIF"
    );
    if lossless {
        let depth = files.iter().filter_map(|file| file.bit_depth).max();
        return Some(match depth {
            Some(depth) if depth > 0 => format!("{extension} {depth}-bit"),
            _ => extension,
        });
    }
    let bitrate = files.iter().filter_map(|file| file.bitrate).min();
    Some(match bitrate {
        Some(bitrate) if bitrate > 0 => format!("{extension} {bitrate}"),
        _ => extension,
    })
}

/// Format from the Newznab category, else from the title.
fn usenet_format(categories: &[i32], title: &str) -> Option<String> {
    if categories.contains(&3040) {
        return Some("FLAC".to_owned());
    }
    let upper = title.to_ascii_uppercase();
    if upper.contains("FLAC") || upper.contains("LOSSLESS") || upper.contains("24BIT") {
        return Some("FLAC".to_owned());
    }
    if upper.contains("320") {
        return Some("MP3 320".to_owned());
    }
    if categories.contains(&3010) {
        return Some("MP3".to_owned());
    }
    None
}

fn extension_of(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| extension.len() <= 5 && !extension.contains(['/', '\\']))
        .unwrap_or_default()
}
