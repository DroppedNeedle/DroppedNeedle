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

use droppedneedle_library::identify::evidence::named_group;
use droppedneedle_library::identify::models::LocalAlbumFacts;
use droppedneedle_library::identify::providers::{IdentifyProviders, LiveProviders};
use droppedneedle_library::identify::service::{IdentifyDeps, IdentifyService, Ranking};
use droppedneedle_library::identify::sources::NoFingerprints;
use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle_library::matching::decide::COMPLETE_TRACK;
use droppedneedle_library::matching::score::LENGTH_GRACE_SECS;
use droppedneedle_library::matching::strings::{basic_dist, undecorated};
use droppedneedle_library::matching::{LocalTrack, Release, ReleaseTrack};
use droppedneedle_providers::executor::{Prepared, Wire, WireAnswer, WireError};
use droppedneedle_providers::musicbrainz::{self, MbSource, MusicBrainzView};
use droppedneedle_providers::{Providers, RequestPriority};
use futures_util::future::BoxFuture;
use serde::Deserialize;

use crate::common::{ScratchDir, migrated_db};

/// The whole corpus must replay within this. It is timed while the rest
/// of the library binary runs beside it: about 10 s on a quiet host and
/// about 30 s on a CI runner, so this catches a slowdown of several
/// times, such as lookups being paced again, not a few seconds.
const BUDGET: Duration = Duration::from_secs(60);

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
    asked: Mutex<Vec<String>>,
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
            asked: Mutex::new(Vec::new()),
            unrecorded: Mutex::new(BTreeSet::new()),
        }
    }
}

impl Wire for Replay {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let url = prepared.url.as_str();
            self.asked.lock().expect("asked list").push(url.to_owned());
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

type Live = LiveProviders<MusicBrainzView, NoFingerprints>;

/// The live recall over the replayed answers, on a fresh store.
fn live_providers(replay: &Arc<Replay>) -> (ScratchDir, Arc<SqliteIdentifyStore>, Arc<Live>) {
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
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ));
    let providers = Arc::new(LiveProviders::new(
        droppedneedle_library::identify::sources::Prioritised::new(
            musicbrainz.clone(),
            musicbrainz,
        ),
        store.clone(),
        NoFingerprints,
    ));
    (dir, store, providers)
}

fn load_albums() -> Vec<Album> {
    serde_json::from_str(
        &std::fs::read_to_string(corpus_path("albums.json")).expect("corpus reads"),
    )
    .expect("corpus parses")
}

/// Recall for one corpus album with its ids stripped and `change`
/// applied: the release groups recalled, and every request asked.
async fn recall_groups(
    key: &str,
    change: impl FnOnce(&mut LocalAlbumFacts),
) -> (Vec<String>, Vec<String>) {
    let album = load_albums()
        .into_iter()
        .find(|album| album.key == key)
        .expect("album in the corpus");
    let mut facts = stripped(&album.facts);
    change(&mut facts);
    let replay = Arc::new(Replay::load());
    let (_dir, _store, providers) = live_providers(&replay);
    let named = named_group(&facts, None);
    let recall = providers
        .recall_candidates(&facts, named.as_deref(), RequestPriority::BackgroundSync)
        .await
        .result;
    let unrecorded = replay.unrecorded.lock().expect("unrecorded list").clone();
    for url in &unrecorded {
        println!("UNRECORDED {url}");
    }
    assert!(unrecorded.is_empty(), "unrecorded requests; see the README");
    let groups = recall
        .releases
        .iter()
        .map(|release| release.release_group_id.clone())
        .collect();
    let asked = replay.asked.lock().expect("asked list").clone();
    (groups, asked)
}

/// An album row that names "Unknown Artist" is searched for under the
/// artist its tracks name, never under the placeholder.
#[tokio::test(flavor = "multi_thread")]
async fn a_placeholder_album_artist_searches_as_its_tracks_artist() {
    let (groups, asked) = recall_groups("crimson-king", |facts| {
        facts.album_artist_name = "Unknown Artist".to_owned();
    })
    .await;
    assert!(
        groups
            .iter()
            .any(|group| group == "a50636b5-5233-3329-a7f3-dba3d0e00ef7"),
        "recall found {groups:?}"
    );
    assert!(
        !asked.iter().any(|url| url.contains("Unknown+Artist")),
        "searched for the placeholder: {asked:?}"
    );
}

/// An album with no title is found through its first track's recording.
#[tokio::test(flavor = "multi_thread")]
async fn an_untitled_album_is_found_by_its_first_track() {
    let (groups, asked) = recall_groups("back-street-crawler-cd1", |facts| {
        facts.title = String::new();
    })
    .await;
    assert!(
        groups
            .iter()
            .any(|group| group == "e3028c11-3abe-3dc1-9e3d-f975858e0b6c"),
        "recall found {groups:?}"
    );
    assert!(asked.iter().any(|url| url.contains("/recording?query=")));
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_albums_match_as_recorded() {
    let started = Instant::now();
    let albums = load_albums();
    let replay = Arc::new(Replay::load());
    let (_dir, store, providers) = live_providers(&replay);
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
            let named = named_group(&facts, None);
            let recall = providers
                .recall_candidates(&facts, named.as_deref(), RequestPriority::BackgroundSync)
                .await
                .result;
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

/// The recording AcoustID heard in its recorded answer
/// (`fixtures/acoustid/batch_lookup.json`: 10cc, "Sand in My Face"),
/// which no release in the corpus carries.
const UNPLACED: &str = "fe000616-c72c-43d4-8cba-a4083e99636b";

/// What AcoustID is made to hear in each file of one copy.
#[derive(Clone, Copy, Debug)]
enum Heard {
    /// A recording no candidate carries, as AcoustID often answers for a
    /// reissue it links to no recording or to an older one.
    Unplaced,
    /// The recording the file pairs with on its release.
    Own,
    /// The recording the next file pairs with: a rival track's.
    Next,
}

/// The recording each file pairs with on `release` in `ranking`, in file
/// order.
fn paired(ranking: &Ranking, release: Option<&str>) -> Vec<(String, String)> {
    let Some(release) = release else {
        return Vec::new();
    };
    ranking
        .candidates
        .iter()
        .find(|candidate| {
            candidate
                .release_mbid
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(release))
        })
        .map(|candidate| {
            candidate
                .track_evidence
                .iter()
                .filter_map(|track| {
                    Some((track.local_track_id.clone(), track.recording_mbid.clone()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The lead candidate pairs some file with another track than its own
/// (the one it pairs with unheard), or leaves it out, while its own track
/// clearly fits it: the title within [`COMPLETE_TRACK`] by `basic_dist`,
/// in full or with bracketed parts and a "feat." tail left out, and the
/// track number or the length (within the length grace) too.
fn moved_off_clear_track(
    ranking: &Ranking,
    releases: &[Release],
    pairs: &[(String, String)],
) -> bool {
    let Some(&lead) = ranking.order.first() else {
        return false;
    };
    let release = &releases[lead];
    let matched = &ranking.matches[lead];
    ranking
        .local
        .tracks
        .iter()
        .enumerate()
        .any(|(index, file)| {
            let Some((_, own)) = pairs.iter().find(|(id, _)| *id == file.id) else {
                return false;
            };
            let Some(own_track) = release
                .tracks
                .iter()
                .find(|track| track.recording_id.eq_ignore_ascii_case(own))
            else {
                return false;
            };
            let stays = matched.pair_for(index).is_some_and(|pair| {
                release.tracks[pair.track]
                    .recording_id
                    .eq_ignore_ascii_case(own)
            });
            !stays && clearly_fits(file, own_track)
        })
}

fn clearly_fits(file: &LocalTrack, track: &ReleaseTrack) -> bool {
    let number = file.track_number > 0
        && (file.track_number == track.absolute_position
            || (file.disc_number.max(1) == track.disc && file.track_number == track.position));
    let length = match (file.duration_secs, track.length_ms) {
        (Some(seconds), Some(ms)) => (seconds - ms as f64 / 1000.0).abs() <= LENGTH_GRACE_SECS,
        _ => false,
    };
    let (bare_file, bare_track) = (undecorated(&file.title), undecorated(&track.title));
    let title = basic_dist(&file.title, &track.title) <= COMPLETE_TRACK
        || (!bare_file.is_empty()
            && !bare_track.is_empty()
            && basic_dist(&bare_file, &bare_track) <= COMPLETE_TRACK);
    title && (number || length)
}

fn heard(
    facts: &LocalAlbumFacts,
    pairs: &[(String, String)],
    heard: Heard,
) -> HashMap<String, Vec<String>> {
    match heard {
        Heard::Unplaced => facts
            .tracks
            .iter()
            .map(|track| (track.local_track_id.clone(), vec![UNPLACED.to_owned()]))
            .collect(),
        Heard::Own => pairs
            .iter()
            .map(|(file, recording)| (file.clone(), vec![recording.clone()]))
            .collect(),
        Heard::Next => pairs
            .iter()
            .enumerate()
            .map(|(index, (file, _))| {
                let next = &pairs[(index + 1) % pairs.len()].1;
                (file.clone(), vec![next.clone()])
            })
            .collect(),
    }
}

/// A fingerprint helps a pair it confirms and counts against one only
/// when what it heard sits on a rival candidate. Heard as a recording no
/// candidate carries, every row, tagged and stripped, stays as recorded.
/// Heard as its own recording, a stripped file never comes out worse.
/// Heard as the next file's recording (a rival track), the lead candidate,
/// confirmed or not, keeps every file on the track it pairs with unheard
/// when that track clearly fits it: audio alone never moves a file off
/// such a track, and settles only files whose titles are unclear. Such an
/// album confirms only when some file's measured length overruled what
/// was heard. Nothing confirms a wrong release group.
#[tokio::test(flavor = "multi_thread")]
async fn fingerprints_confirm_and_never_veto() {
    let albums = load_albums();
    let replay = Arc::new(Replay::load());
    let (_dir, store, providers) = live_providers(&replay);
    let service = IdentifyService::new(IdentifyDeps {
        store,
        providers: providers.clone(),
    });
    let mut failures = Vec::new();
    let mut tallies: Vec<(String, Tally)> = Vec::new();
    for album in &albums {
        let copies = [
            ("tagged", album.facts.clone(), &album.tagged),
            ("stripped", stripped(&album.facts), &album.stripped),
        ];
        for (copy, facts, expected) in copies {
            let named = named_group(&facts, None);
            let recall = providers
                .recall_candidates(&facts, named.as_deref(), RequestPriority::BackgroundSync)
                .await
                .result;
            let unheard = service.rank(&facts, &recall).expect("default preferences");
            // The true release, or where the truth names no edition, the
            // one this copy was recorded as confirming in the true group.
            let release = album.truth.release.as_deref().or(expected
                .release
                .as_deref()
                .filter(|_| expected.verdict == Verdict::Group));
            let pairs = paired(&unheard, release);
            let scenarios: &[Heard] = if copy == "tagged" || pairs.is_empty() {
                &[Heard::Unplaced]
            } else if pairs.len() < 2 {
                &[Heard::Unplaced, Heard::Own]
            } else {
                &[Heard::Unplaced, Heard::Own, Heard::Next]
            };
            for scenario in scenarios {
                let mut recall = recall.clone();
                let heard = heard(&facts, &pairs, *scenario);
                recall.fingerprint_support = heard.clone();
                let ranking = service.rank(&facts, &recall).expect("default preferences");
                let (got, reason) = outcome(&ranking, &album.truth);
                let overruled = ranking
                    .order
                    .first()
                    .map_or(0, |lead| ranking.matches[*lead].overruled.len());
                let label = format!("{copy} heard {scenario:?}");
                match tallies.iter_mut().find(|(name, _)| *name == label) {
                    Some((_, tally)) => tally.add(got.verdict),
                    None => {
                        let mut tally = Tally::default();
                        tally.add(got.verdict);
                        tallies.push((label.clone(), tally));
                    }
                }
                println!(
                    "{:<28} {label:<24} expected {:<10} {:<8}  got {:<10} {:<8} {reason} \
                     overruled {overruled}",
                    album.key,
                    expected.verdict.label(),
                    short(expected.release.as_deref()),
                    got.verdict.label(),
                    short(got.release.as_deref()),
                );
                let fails = got.verdict == Verdict::Wrong
                    || match scenario {
                        Heard::Unplaced => {
                            got.verdict != expected.verdict || got.release != expected.release
                        }
                        Heard::Own => got.verdict > expected.verdict,
                        Heard::Next => {
                            moved_off_clear_track(&ranking, &recall.releases, &pairs)
                                || (got.verdict <= Verdict::Group && overruled == 0)
                        }
                    };
                if fails {
                    failures.push(format!(
                        "{} ({label}): expected {} {}, got {} {} ({reason})",
                        album.key,
                        expected.verdict.label(),
                        expected.release.as_deref().unwrap_or("-"),
                        got.verdict.label(),
                        got.release.as_deref().unwrap_or("-"),
                    ));
                }
            }
        }
    }
    for (label, tally) in &tallies {
        println!("{}", tally.line(label));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
