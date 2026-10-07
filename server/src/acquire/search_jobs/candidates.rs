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

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::acquire::plugin_source::{Band, ScoredRelease, release_identity};
use crate::acquire::slskd::SearchResult;
use crate::acquire::slskd::folders::FolderPick;
use crate::acquire::target::SearchTarget;
use crate::acquire::target::releases::release_fit;
use crate::acquire::usenet::newznab::{IndexerResult, usenet_identity};

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
}

/// Why a candidate sits where it does: one stable code and one sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CandidateNote {
    /// Stable code (`complete`, `incomplete`, `lengths`, `quality`,
    /// `no_tracklist`, `names_album`, `title_unclear`, `strong`, `weak`).
    pub code: String,
    /// What the candidate looks like, in one sentence.
    pub text: String,
}

impl CandidateNote {
    fn new(code: &str, text: String) -> Self {
        Self {
            code: code.to_owned(),
            text,
        }
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
    /// Tracks on the edition, when known.
    pub tracks_total: Option<usize>,
    /// Format label (`FLAC`, `MP3 320`, `lossless`), when known.
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
    /// The identity the wanted watcher remembers this candidate by.
    pub fn seen_identity(&self) -> String {
        match &self.pin {
            Pin::Soulseek {
                username, folder, ..
            } => format!("{username}/{folder}"),
            Pin::Release { identity } => identity.clone(),
        }
    }

    /// A ranked Soulseek folder.
    pub fn from_folder(pick: FolderPick) -> Self {
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
        Self {
            view: SearchCandidateView {
                candidate_index: 0,
                source: "soulseek".to_owned(),
                title,
                tier,
                note,
                username: Some(pick.username),
                file_count: files.len(),
                files,
                size_bytes,
                tracks_matched: (pick.total > 0).then_some(pick.coverage),
                tracks_total: (pick.total > 0).then_some(pick.total),
                format,
                has_free_slot: Some(has_free_slot),
                queue_length,
                upload_speed,
                indexer: None,
                posted_at: None,
                grabs: None,
            },
            pin,
        }
    }

    /// A Usenet release, judged on its title and file count.
    pub fn from_release(hit: &IndexerResult, target: &SearchTarget) -> Self {
        let fit = release_fit(hit, target);
        let (tier, note) = match fit.names {
            0 => (
                CandidateTier::Recommended,
                CandidateNote::new(
                    "names_album",
                    "The release title names this album and artist.".to_owned(),
                ),
            ),
            1 => (
                CandidateTier::Possible,
                CandidateNote::new(
                    "title_unclear",
                    "The release title names this album but not the artist.".to_owned(),
                ),
            ),
            _ => (
                CandidateTier::Possible,
                CandidateNote::new(
                    "title_unclear",
                    "The release title does not clearly name this album.".to_owned(),
                ),
            ),
        };
        let release = &hit.usenet;
        Self {
            view: SearchCandidateView {
                candidate_index: 0,
                source: "usenet".to_owned(),
                title: release.title.clone(),
                tier,
                note,
                username: None,
                files: Vec::new(),
                file_count: release
                    .files
                    .and_then(|files| usize::try_from(files).ok())
                    .unwrap_or(0),
                size_bytes: release.size_bytes,
                tracks_matched: None,
                tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
                format: usenet_format(&release.category_ids, &release.title),
                has_free_slot: None,
                queue_length: None,
                upload_speed: None,
                indexer: Some(release.indexer_name.clone()),
                posted_at: release.usenet_date,
                grabs: release.grabs,
            },
            pin: Pin::Release {
                identity: usenet_identity(&release.title, release.size_bytes),
            },
        }
    }

    /// A plugin release, judged by the plugin's own score.
    pub fn from_plugin(source: &str, scored: ScoredRelease) -> Self {
        let (tier, note) = if scored.band == Band::Auto {
            (
                CandidateTier::Recommended,
                CandidateNote::new("strong", "The source rates this a strong match.".to_owned()),
            )
        } else {
            (
                CandidateTier::Possible,
                CandidateNote::new(
                    "weak",
                    "The source rates this a weak match. Check the title before picking."
                        .to_owned(),
                ),
            )
        };
        let release = scored.release;
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
                source: source.to_owned(),
                title: release.title.clone(),
                tier,
                note,
                username,
                file_count: files.len(),
                files,
                size_bytes: u64::try_from(release.size_bytes.max(0)).unwrap_or(0),
                tracks_matched: None,
                tracks_total: None,
                format: (!release.quality_tier.is_empty()).then(|| release.quality_tier.clone()),
                has_free_slot: None,
                queue_length: None,
                upload_speed: None,
                indexer: None,
                posted_at: release.usenet_date,
                grabs: None,
            },
        }
    }
}

/// Tier and note for one folder: the first thing wrong with it, or that
/// it looks complete.
fn folder_note(pick: &FolderPick) -> (CandidateTier, CandidateNote) {
    let possible =
        |code: &str, text: String| (CandidateTier::Possible, CandidateNote::new(code, text));
    if pick.total == 0 {
        return possible(
            "no_tracklist",
            format!(
                "{} audio files. The edition's tracklist could not be read to compare them.",
                pick.coverage
            ),
        );
    }
    if pick.coverage < pick.total {
        return possible(
            "incomplete",
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
        return possible("lengths", text);
    }
    if pick.quality == usize::MAX {
        return possible(
            "quality",
            "Some files are outside your quality settings.".to_owned(),
        );
    }
    (
        CandidateTier::Recommended,
        CandidateNote::new(
            "complete",
            format!("Holds all {} tracks of this edition.", pick.total),
        ),
    )
}

/// `FLAC`, `MP3 320` and so on, from the folder's files.
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
    let bitrate = files.iter().filter_map(|file| file.bitrate).min();
    Some(match bitrate {
        Some(bitrate) if !lossless && bitrate > 0 => format!("{extension} {bitrate}"),
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
