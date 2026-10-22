//! Matching quality, replayed: real albums from a test library, run
//! through the live recall, the scoring and the decision, compared with
//! the result each album is recorded to get.
//!
//! `fixtures/matching_corpus/albums.json` holds each album's facts as
//! identification reads them (file paths and ids replaced), the release
//! it truly is, and its recorded result twice: as tagged, and as a
//! stripped copy with every MusicBrainz id removed, which measures the
//! untagged path. `musicbrainz.jsonl` holds every MusicBrainz answer
//! recall asks for, keyed by request URL, replayed through the shipping
//! client at the HTTP seam. No network.
//!
//! A row that comes out worse fails. A row that comes out better fails
//! too until it is recorded, so every fix shows the rows it flips. A row
//! short of the truth names why in `known_miss`, and only such a row. The README beside the
//! fixtures says how to record answers for new requests.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droppedneedle_library::identify::models::LocalAlbumFacts;
use droppedneedle_library::identify::providers::{IdentifyProviders, LiveProviders};
use droppedneedle_library::identify::service::{IdentifyDeps, IdentifyService, Ranking};
use droppedneedle_library::identify::sources::NoFingerprints;
use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle_providers::executor::{Prepared, Wire, WireAnswer, WireError};
use droppedneedle_providers::musicbrainz::{self, MbSource};
use droppedneedle_providers::{Providers, RequestPriority};
use futures_util::future::BoxFuture;
use serde::Deserialize;

use crate::common::{ScratchDir, migrated_db};

/// The whole corpus must replay within this.
const BUDGET: Duration = Duration::from_secs(30);

fn corpus_path(name: &str) -> String {
    format!(
        "{}/tests/fixtures/matching_corpus/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[derive(Deserialize)]
struct Album {
    key: String,
    why: String,
    truth: Truth,
    facts: LocalAlbumFacts,
    tagged: Row,
    stripped: Row,
}

/// What the album truly is. `release` is `None` when the files cannot
/// tell its editions apart.
#[derive(Deserialize)]
struct Truth {
    release_group: String,
    release: Option<String>,
}

/// One recorded result.
#[derive(Deserialize, Clone, PartialEq)]
struct Row {
    verdict: Verdict,
    /// The release sealed or guessed; `None` when nothing was.
    release: Option<String>,
    /// Why a row falls short of the truth.
    #[serde(default)]
    known_miss: Option<String>,
}

/// How an album came out, best first.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    /// Confirmed on the true release.
    Exact,
    /// Confirmed in the true release group, not on the true release (or
    /// the true release is not known).
    Group,
    /// A best guess sealed unconfirmed, for a person to check.
    Review,
    /// Nothing sealed: the album keeps its own tags.
    Unmatched,
    /// Confirmed on another release group.
    Wrong,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Group => "group only",
            Self::Review => "review",
            Self::Unmatched => "unmatched",
            Self::Wrong => "wrong",
        }
    }
}

/// One recorded MusicBrainz answer.
#[derive(Deserialize)]
struct Answer {
    url: String,
    status: u16,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    body: Option<serde_json::Value>,
}

/// MusicBrainz replayed from the recorded answers. A request with no
/// answer is noted; a lookup then finds nothing and a search an empty
/// page, so recall carries on and one run lists every missing answer.
struct Replay {
    answers: HashMap<String, WireAnswer>,
    unrecorded: Mutex<BTreeSet<String>>,
}

impl Replay {
    fn load() -> Self {
        let text = std::fs::read_to_string(corpus_path("musicbrainz.jsonl"))
            .expect("recorded answers read");
        let answers = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let answer: Answer = serde_json::from_str(line).expect("recorded answer parses");
                let headers = answer
                    .location
                    .map(|location| vec![("location".to_owned(), location)])
                    .unwrap_or_default();
                let body = answer
                    .body
                    .map(|body| serde_json::to_vec(&body).expect("body serializes"))
                    .unwrap_or_default();
                (
                    answer.url,
                    WireAnswer {
                        status: answer.status,
                        headers,
                        body,
                    },
                )
            })
            .collect();
        Self {
            answers,
            unrecorded: Mutex::new(BTreeSet::new()),
        }
    }
}

impl Wire for Replay {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let url = prepared.url.as_str();
            if let Some(answer) = self.answers.get(url) {
                return Ok(answer.clone());
            }
            self.unrecorded
                .lock()
                .expect("unrecorded list")
                .insert(url.to_owned());
            let search = prepared.url.query_pairs().any(|(name, _)| name == "query");
            Ok(if search {
                WireAnswer {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"releases":[],"count":0,"offset":0}"#.to_vec(),
                }
            } else {
                WireAnswer {
                    status: 404,
                    headers: Vec::new(),
                    body: br#"{"error":"Not Found"}"#.to_vec(),
                }
            })
        })
    }
}

/// The album with every MusicBrainz id taken off its files.
fn stripped(facts: &LocalAlbumFacts) -> LocalAlbumFacts {
    let mut facts = facts.clone();
    for track in &mut facts.tracks {
        track.recording_mbid = None;
        track.release_track_mbid = None;
        track.release_mbid = None;
        track.release_group_mbid = None;
    }
    facts
}

/// What a queued attempt would do with this ranking, as a row.
fn outcome(ranking: &Ranking, truth: &Truth) -> (Row, String) {
    let reason = ranking.reason_code.clone();
    let (confirmed, pick) = if ranking.lead_identified {
        (true, ranking.candidates.first())
    } else {
        (
            false,
            ranking
                .guess
                .and_then(|index| ranking.candidates.get(index)),
        )
    };
    let Some(pick) = pick else {
        return (
            Row {
                verdict: Verdict::Unmatched,
                release: None,
                known_miss: None,
            },
            reason,
        );
    };
    let release = pick.release_mbid.clone().map(|id| id.to_ascii_lowercase());
    let verdict = if !confirmed {
        Verdict::Review
    } else if truth.release.is_some() && truth.release == release {
        Verdict::Exact
    } else if pick
        .release_group_mbid
        .eq_ignore_ascii_case(&truth.release_group)
    {
        Verdict::Group
    } else {
        Verdict::Wrong
    };
    (
        Row {
            verdict,
            release,
            known_miss: None,
        },
        reason,
    )
}

fn short(release: Option<&str>) -> &str {
    release.map_or("-", |id| id.get(..8).unwrap_or(id))
}

#[derive(Default)]
struct Tally([usize; 5]);

impl Tally {
    fn add(&mut self, verdict: Verdict) {
        self.0[verdict as usize] += 1;
    }

    fn line(&self, copy: &str) -> String {
        let [exact, group, review, unmatched, wrong] = self.0;
        format!(
            "{copy}: exact {exact}, group only {group}, review {review}, \
             unmatched {unmatched}, wrong {wrong}"
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_albums_match_as_recorded() {
    let started = Instant::now();
    let albums: Vec<Album> = serde_json::from_str(
        &std::fs::read_to_string(corpus_path("albums.json")).expect("corpus reads"),
    )
    .expect("corpus parses");
    let replay = Arc::new(Replay::load());
    let musicbrainz = Providers::unpaced(replay.clone()).musicbrainz.fixed(
        MbSource::BrainzMash {
            base_url: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
            binding_valid: true,
        },
        RequestPriority::BackgroundSync,
    );
    let dir = ScratchDir::new("matching-corpus");
    let path = dir.join("app.db");
    migrated_db(&path);
    let store = Arc::new(SqliteIdentifyStore::new(
        droppedneedle_kernel::db::Db::open_for_tests(&path).expect("store opens"),
    ));
    let providers = Arc::new(LiveProviders::new(
        musicbrainz,
        store.clone(),
        NoFingerprints,
    ));
    let service = IdentifyService::new(IdentifyDeps {
        store,
        providers: providers.clone(),
    });

    let mut failures = Vec::new();
    let mut table = Vec::new();
    let mut tallies = [Tally::default(), Tally::default()];
    for album in &albums {
        assert!(
            !album.why.is_empty(),
            "{}: say why the album is here",
            album.key
        );
        let copies = [
            ("tagged", album.facts.clone(), &album.tagged),
            ("stripped", stripped(&album.facts), &album.stripped),
        ];
        for (slot, (copy, facts, expected)) in copies.into_iter().enumerate() {
            let recall = providers.recall_candidates(&facts).await.result;
            if recall.provider_deferred {
                failures.push(format!("{} ({copy}): recall deferred", album.key));
                continue;
            }
            let ranking = service.rank(&facts, &recall).expect("default preferences");
            let (got, reason) = outcome(&ranking, &album.truth);
            tallies[slot].add(got.verdict);
            table.push(format!(
                "{:<28} {copy:<8} expected {:<10} {:<8}  got {:<10} {:<8} {reason}",
                album.key,
                expected.verdict.label(),
                short(expected.release.as_deref()),
                got.verdict.label(),
                short(got.release.as_deref()),
            ));
            let short_of_truth = match expected.verdict {
                Verdict::Exact => false,
                Verdict::Group => album.truth.release.is_some(),
                _ => true,
            };
            if short_of_truth && expected.known_miss.is_none() {
                failures.push(format!(
                    "{} ({copy}): a row short of the truth needs a known_miss",
                    album.key
                ));
            }
            if !short_of_truth && expected.known_miss.is_some() {
                failures.push(format!(
                    "{} ({copy}): a row at the truth drops its known_miss",
                    album.key
                ));
            }
            let change = if got.verdict > expected.verdict {
                Some("worse than recorded")
            } else if got.verdict < expected.verdict {
                Some("better than recorded: record the new row")
            } else if got.release != expected.release {
                Some("a different release than recorded")
            } else {
                None
            };
            if let Some(change) = change {
                failures.push(format!(
                    "{} ({copy}): {change}: expected {} {}, got {} {} ({reason})",
                    album.key,
                    expected.verdict.label(),
                    expected.release.as_deref().unwrap_or("-"),
                    got.verdict.label(),
                    got.release.as_deref().unwrap_or("-"),
                ));
            }
        }
    }
    let elapsed = started.elapsed();
    println!("{}", table.join("\n"));
    println!("{}", tallies[0].line("tagged"));
    println!("{}", tallies[1].line("stripped"));
    println!("{} albums in {elapsed:?}", albums.len());

    let unrecorded = replay.unrecorded.lock().expect("unrecorded list").clone();
    for url in &unrecorded {
        println!("UNRECORDED {url}");
    }
    assert!(
        unrecorded.is_empty(),
        "{} MusicBrainz requests have no recorded answer; see the README",
        unrecorded.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(elapsed < BUDGET, "the corpus took {elapsed:?}");
}
