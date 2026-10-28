//! Library journeys.
//!
//! The journeys run the production app on a scratch data directory
//! ([`TestApp`]) with MusicBrainz and AcoustID scripted at the two provider
//! ports. The background loops run as they do in production, so a journey
//! waits on what it can observe: a scan run's state, the identification
//! queue, a job's state. Rows are read and seeded through connections of
//! the journey's own to the app's database. Every folder is a fresh sandbox
//! under the scratch directory, planted from read-only fixture copies.
//!
//! The tests on `bare_library` are component tests: they step a worker or a
//! store by hand, on a library bundle with no app around it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common::library::TestSql as _;
use crate::common::library::{ScriptedContrib, ScriptedIdentify, library_asking, library_with};
use crate::common::{ScratchDir, TestApp, closed_outbound};
use axum::http::StatusCode;
use droppedneedle::bootstrap::Port;
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::store::play_marks::{MarkScope, PlayMarkStore};
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
use droppedneedle_library::identify::models::{LocalAlbumFacts, RecallResult};
use droppedneedle_library::identify::providers::FakeProviders;
use droppedneedle_library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle_library::management::automatic::{SETTLE_SECONDS, Settle};
use droppedneedle_library::management::models::ResultState;
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};
use droppedneedle_library::scan::SqliteScanStore;
use droppedneedle_library::wiring::LibrarySetup;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The two tied editions the scripted recall offers. Real release MBIDs,
/// because choosing an edition checks the ID's shape.
const REL_1: &str = "11111111-1111-4111-8111-111111111111";
const REL_2: &str = "22222222-2222-4222-8222-222222222222";

/// The app for one journey, the provider scripts it answers from, and the
/// journey's own connections to its database.
struct Rig {
    app: TestApp,
    identify: Arc<ScriptedIdentify>,
    contrib: ScriptedContrib,
    scan_store: SqliteScanStore,
    identify_store: SqliteIdentifyStore,
    /// Per album, when the last identification job the journey accounted
    /// for ended.
    settled_at: RefCell<HashMap<String, i64>>,
}

impl Rig {
    async fn start(tag: &str) -> Self {
        let identify = ScriptedIdentify::new();
        let contrib = ScriptedContrib::default();
        let mut outbound = closed_outbound();
        outbound.identify_providers = Port::given(identify.clone());
        outbound.contribution_providers = Port::given(contrib.providers());
        let app = TestApp::start_with(tag, outbound).await;
        let db = droppedneedle_kernel::db::Db::open_for_tests(&app.config.library_db_path)
            .expect("database opens");
        Self {
            scan_store: SqliteScanStore::new(
                db.clone(),
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            identify_store: SqliteIdentifyStore::new(
                db,
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            app,
            identify,
            contrib,
            settled_at: RefCell::default(),
        }
    }

    /// End the journey: the app stops as it does in production, and the
    /// harness fails the journey if a background loop held that up.
    async fn stop(self) {
        self.app.stop().await;
    }

    /// Stop the app and build it again on the same data directory.
    async fn restart(self) -> Self {
        Self {
            app: self.app.restart().await,
            ..self
        }
    }

    /// Pause identification as the owner. Returns the switch's revision.
    async fn pause_identification(&self) -> u64 {
        let auth = self.app.login_admin().await;
        let headers = [("authorization", auth.as_str())];
        let (status, body) = self
            .app
            .call("GET", "/api/v3/library/activity", &headers, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let control = control_revision(&body);
        let (status, body) = self
            .app
            .call(
                "POST",
                "/api/v3/library/identification/pause",
                &headers,
                Some(json!({"expected_revision": control})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["row_revision"].as_u64().expect("revision")
    }

    /// Let identification run again; the loop wakes at once. Returns the
    /// switch's revision.
    async fn resume_identification(&self, paused_at: u64) -> u64 {
        let auth = self.app.login_admin().await;
        let (status, body) = self
            .app
            .call(
                "POST",
                "/api/v3/library/identification/resume",
                &[("authorization", auth.as_str())],
                Some(json!({"expected_revision": paused_at})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["row_revision"].as_u64().expect("revision")
    }

    /// The owner chooses `REL_1` as the album's edition.
    async fn choose_first_edition(&self, album: &str) {
        let auth = self.app.login_admin().await;
        let (status, body) = self
            .app
            .call(
                "PUT",
                &format!("/api/v3/library/albums/{album}/edition"),
                &[("authorization", auth.as_str())],
                Some(json!({"release_mbid": REL_1})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Wait until the album has no identification job open, and check that
    /// exactly `jobs` of its jobs ended since the journey last asked. Jobs
    /// of other albums are not counted.
    async fn identified(&self, album: &str, jobs: i64) {
        let since = self.settled_at.borrow().get(album).copied().unwrap_or(0);
        let db = self.app.db();
        let (ended, latest) = self
            .app
            .until("the album's identification settles", || {
                let (open, ended, latest): (i64, i64, i64) = db
                    .query_row(
                        "SELECT COALESCE(SUM(state IN ('queued', 'running')), 0), \
                         COALESCE(SUM(state NOT IN ('queued', 'running') AND updated_ms > ?1), 0), \
                         COALESCE(MAX(updated_ms), 0) FROM library_identify_jobs \
                         WHERE local_album_id = ?2",
                        rusqlite::params![since, album],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .expect("identify queue reads");
                async move { (open == 0 && ended >= jobs).then_some((ended, latest)) }
            })
            .await;
        assert_eq!(ended, jobs, "identification jobs that ran");
        self.settled_at
            .borrow_mut()
            .insert(album.to_owned(), latest);
    }

    /// Wait until the album has no identification job open.
    async fn identification_idle(&self, album: &str) {
        let db = self.app.db();
        self.app
            .until("the album's identification is idle", || {
                let open: i64 = db
                    .query_row(
                        "SELECT COUNT(*) FROM library_identify_jobs \
                         WHERE local_album_id = ?1 AND state IN ('queued', 'running')",
                        [album],
                        |row| row.get(0),
                    )
                    .expect("identify queue reads");
                async move { (open == 0).then_some(()) }
            })
            .await;
    }

    /// Wait until the app is done with the edition a person chose for the
    /// album. Approving a candidate that names only a release queues the
    /// placing of the album's files for the operations loop. When it gets
    /// there it places them, or hands the album back to automatic choice,
    /// and either way moves the album's identity and the catalog revision:
    /// a preview sealed before then is stale, and an Undo after it is
    /// blocked. A manage preview asked for before then is refused with the
    /// message a person sees, so a journey waits here before it previews.
    async fn edition_settled(&self, album: &str) {
        let db = self.app.db();
        self.app
            .until("the chosen edition is placed or handed back", || {
                let waiting: i64 = db
                    .query_row(
                        "SELECT COUNT(*) FROM library_edition_remap_queue \
                         WHERE local_album_id = ?1",
                        [album],
                        |row| row.get(0),
                    )
                    .expect("remap queue reads");
                async move { (waiting == 0).then_some(()) }
            })
            .await;
    }
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library")
}

/// Copy one committed fixture into a sandbox root. The fixture dir is
/// only ever read; the sandbox copy is the scan target.
fn plant(root: &Path, relative: &str, fixture: &str) -> PathBuf {
    let dest = root.join(relative);
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixtures_dir().join(fixture), &dest).expect("copy fixture");
    dest
}

/// Byte-level snapshot of a root for the purity pin: relative path
/// to (size, mtime_ns, sha256), matching the scan briefs. mtime is
/// pinned because a writer that "restores" content still moves it.
fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<String, (u64, i64, String)> {
    let mut snapshot = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("read sandbox dir");
        for entry in entries {
            let entry = entry.expect("sandbox entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("sandbox prefix")
                .to_string_lossy()
                .into_owned();
            // Publisher metadata lives under the hidden prefix; the
            // purity pin covers media files only.
            if relative
                .split('/')
                .any(|part| part.starts_with(".droppedneedle-management-"))
            {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read sandbox file");
            let meta = std::fs::metadata(&path).expect("stat sandbox file");
            let mtime_ns = droppedneedle_library::scan::mtime_ns_from_metadata(&meta);
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            snapshot.insert(relative, (bytes.len() as u64, mtime_ns, hex));
        }
    }
    snapshot
}

/// Drive supervisor ticks until no run is current (cap 100).
async fn drain_scans(library: &LibrarySetup) {
    library.scans.scan_startup_recovery().await;
    for _ in 0..100 {
        library.scans.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    assert!(
        library.coordinator.current().is_empty(),
        "scans drain to idle"
    );
}

/// Two release groups that fit the album's files equally well, built
/// from the facts identification reads: the attempt must file an
/// ambiguous review, never guess. Both answer to the release MBID the
/// files carry (as if MusicBrainz had folded it into each), so the
/// album id cannot pick one.
fn ambiguous_recall(store: &SqliteIdentifyStore, album_id: &str) -> RecallResult {
    ambiguous_from(&store.album_facts(album_id).unwrap().expect("album facts"))
}

/// [`ambiguous_recall`] for the album identification is asking about.
fn ambiguous_from(facts: &LocalAlbumFacts) -> RecallResult {
    let tagged: Vec<String> = facts
        .tracks
        .iter()
        .filter_map(|track| track.release_mbid.clone())
        .collect();
    let release = |group: &str, id: &str| Release {
        id: id.to_owned(),
        release_group_id: group.to_owned(),
        title: facts.title.clone(),
        artists: vec![CreditedArtist {
            id: "artist-1".to_owned(),
            name: facts.album_artist_name.clone(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: facts
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: track
                    .release_track_mbid
                    .clone()
                    .unwrap_or_else(|| format!("{id}-rt-{index}")),
                recording_id: track
                    .recording_mbid
                    .clone()
                    .unwrap_or_else(|| format!("rec-{index}")),
                title: track.title.clone(),
                artists: Vec::new(),
                disc: track.disc_number.max(1),
                position: track.track_number,
                absolute_position: index as u32 + 1,
                length_ms: track.duration_secs.map(|seconds| seconds * 1000),
            })
            .collect(),
        old_ids: tagged.clone(),
        ..Release::default()
    };
    RecallResult {
        releases: vec![release("rg-1", REL_1), release("rg-2", REL_2)],
        ..RecallResult::default()
    }
}

fn read_title(path: &Path) -> String {
    droppedneedle_library::tags::read::read_tag_only(
        path,
        droppedneedle_library::tags::AudioFormat::Flac,
    )
    .expect("tags read")
    .title
}

/// File the pending review an edition correction leaves when it drops an
/// album's editions (a merge that detaches conflicting editions): the
/// dropped editions are the candidates, keyed `group:release`.
/// Identification itself never files one; it always picks a best guess.
fn file_edition_review(store: &SqliteIdentifyStore, album_id: &str) -> String {
    use droppedneedle_library::identify::models::CandidateEvidence;
    let candidate = |group: &str, release: &str| CandidateEvidence {
        candidate_key: format!("{group}:{release}"),
        release_group_mbid: group.to_owned(),
        release_mbid: Some(release.to_owned()),
        album_title: "Album".to_owned(),
        album_artist_name: String::new(),
        track_evidence: Vec::new(),
        score: 0.0,
        reason_code: "EDITION_CLEARED_CONFLICT".to_owned(),
        distance: 0.0,
        penalties: Vec::new(),
    };
    let id = format!("rev-{album_id}");
    droppedneedle_library::identify::review::file_review(
        store,
        &id,
        album_id,
        "EDITION_CLEARED_CONFLICT",
        vec![candidate("rg-1", REL_1), candidate("rg-2", REL_2)],
    )
    .unwrap();
    id
}

#[tokio::test]
async fn library_journey_scan_identify_review_organize_undo() {
    let rig = Rig::start("lib-organize").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];

    // Scripted providers tie two candidates for whatever album is asked
    // about.
    rig.identify.recall_from(ambiguous_from);

    // Plant two tracks in one album dir, then add the root.
    let music = app.config.root_app_dir.join("music");
    let file_a = plant(&music, "album-a/01.flac", "management_full.flac");
    let file_b = plant(&music, "album-a/02.flac", "management_full.flac");
    let title_a_before = read_title(&file_a);
    let title_b_before = read_title(&file_b);
    let bytes_before = snapshot_tree(&music);
    let first_scan = app.add_library_folder("music", &music).await;
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/scan/runs/{first_scan}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["state"], json!("completed"));
    assert_eq!(
        body["run"]["counters"]["indexed_count"].as_i64(),
        Some(2),
        "exactly the two planted tracks index"
    );
    let (track_a, track_b) = {
        let track = |path| {
            rig.scan_store
                .track_at("music", path)
                .unwrap()
                .expect("track indexed")
        };
        (track("album-a/01.flac"), track("album-a/02.flac"))
    };

    // Purity: the scan wrote zero media bytes.
    assert_eq!(snapshot_tree(&music), bytes_before);
    assert_eq!(read_title(&file_a), title_a_before);
    assert_eq!(read_title(&file_b), title_b_before);

    // Identification keys on the catalog album the reads API serves.
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{track_a}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let album_id = body["album_id"].as_str().expect("album id").to_owned();
    let album_key = album_id.as_str();

    // The scan-enqueued job runs once. On a tie it still picks a best
    // guess (always pick) and files no review.
    rig.identified(album_key, 1).await;
    // Purity extends past identify: tag reads and probes wrote zero
    // media bytes too.
    assert_eq!(
        snapshot_tree(&music),
        bytes_before,
        "identify reads wrote files"
    );
    {
        assert!(
            rig.identify_store
                .album_identity(album_key)
                .unwrap()
                .is_some(),
            "identification picks a best guess"
        );
    }
    {
        assert!(
            rig.identify_store
                .pending_for_album(album_key)
                .unwrap()
                .is_empty()
        );
    }
    // A correction that drops the album's editions leaves a review.
    file_edition_review(&rig.identify_store, album_key);
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/reviews?album_id={album_key}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviews = body["reviews"].as_array().expect("reviews");
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0]["reason_code"], json!("EDITION_CLEARED_CONFLICT"));
    assert_eq!(
        reviews[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        2
    );
    let review_id = reviews[0]["id"].as_str().expect("review id").to_owned();

    // A manual re-enqueue is accepted (job queued); it runs against an
    // empty recall, so it ends without touching the review.
    rig.identify.set_recall(RecallResult::default());
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/identify",
            &headers,
            Some(json!({"album_id": album_key, "kind": "manual"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.identified(album_key, 1).await;

    // Curator approval seals a manual identity. The candidate names only
    // the release, so the app goes on to ask MusicBrainz for it. The script
    // was emptied above and does not know it, so the album goes back to
    // automatic choice on the same release. The preview waits for that.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/reviews/{review_id}/approve"),
            &headers,
            Some(json!({"candidate_key": format!("rg-1:{REL_1}")})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["review"]["state"], json!("approved"));
    assert_eq!(body["identity"]["release_mbid"], json!(REL_1));
    assert_eq!(body["identity"]["decision_source"], json!("manual"));
    rig.edition_settled(album_key).await;

    // Auto-organize the whole album: preview, apply, and the files move
    // with new tags.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(json!({
                "kind": "organize",
                "album_id": album_key,
                "items": [
                    {
                        "root_id": "music",
                        "rel_path": "album-a/01.flac",
                        "dest_rel": "organized/01.flac",
                        "managed_updates": {"title": ["Organized Title"]},
                    },
                    {
                        "root_id": "music",
                        "rel_path": "album-a/02.flac",
                        "dest_rel": "organized/02.flac",
                        "managed_updates": {"title": ["Organized Title"]},
                    },
                ],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["preview_token"].as_str().expect("token").to_owned();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &headers,
            Some(json!({"preview_token": token})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], json!("committed"));
    let bundle_id = body["bundle_id"].as_str().expect("bundle").to_owned();
    let moved = music.join("organized/01.flac");
    assert!(moved.is_file(), "organized file lands");
    assert!(!file_a.exists(), "source leaves after commit");
    assert_eq!(read_title(&moved), "Organized Title");

    // A rescan after the move re-reads the rewritten files and keeps them
    // on the same album, so the approved release still applies. The rescan
    // queues the album for identification again; Undo checks the identity,
    // so that job ends first.
    app.scan_library().await;
    rig.identified(album_key, 1).await;
    {
        for track in [&track_a, &track_b] {
            assert_eq!(
                rig.scan_store.album_for_track(track).unwrap().as_deref(),
                Some(album_key),
                "an organized track stays on its album"
            );
        }
        let identity = rig
            .identify_store
            .album_identity(album_key)
            .unwrap()
            .expect("identity stays with the album");
        assert_eq!(identity.release_mbid.as_deref(), Some(REL_1));
    }

    // Undo restores the exact before state: path and tags.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/undo",
            &headers,
            Some(json!({"bundle_id": bundle_id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut restored: Vec<String> =
        serde_json::from_value(body["restored"].clone()).expect("restored ids");
    restored.sort();
    let mut expected = vec![track_a.clone(), track_b.clone()];
    expected.sort();
    assert_eq!(restored, expected);
    assert!(file_a.is_file(), "source returns after undo");
    assert!(!moved.exists(), "destination clears after undo");
    assert_eq!(read_title(&file_a), title_a_before);
    rig.stop().await;
}

#[tokio::test]
async fn library_journey_retag_apply_baseline_restore() {
    let rig = Rig::start("lib-retag").await;
    rig.identify.recall_from(ambiguous_from);
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];

    let music = app.config.root_app_dir.join("music");
    let file = plant(&music, "album-b/01.flac", "management_full.flac");
    let original_title = read_title(&file);
    let fields_before = droppedneedle_library::tags::read_fields(&file).expect("fields read");
    app.add_library_folder("music", &music).await;
    let track_id = {
        rig.scan_store
            .track_at("music", "album-b/01.flac")
            .unwrap()
            .expect("track indexed")
    };

    // Identify (it picks a best guess), then approve the winner of the
    // review a dropped edition leaves.
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{track_id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let album_id = body["album_id"].as_str().expect("album id").to_owned();
    let album_key = album_id.as_str();
    rig.identified(album_key, 1).await;
    file_edition_review(&rig.identify_store, album_key);
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/reviews?album_id={album_key}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let review_id = body["reviews"][0]["id"]
        .as_str()
        .expect("review")
        .to_owned();
    let (status, _) = app
        .call(
            "POST",
            &format!("/api/v3/library/reviews/{review_id}/approve"),
            &headers,
            Some(json!({"candidate_key": format!("rg-1:{REL_1}")})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // The candidate names only the release: the app places the file on it
    // in its own time, and the preview is sealed after that.
    rig.edition_settled(album_key).await;

    // Manual retag preview, then apply.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(json!({
                "kind": "retag",
                "album_id": album_key,
                "items": [{
                    "root_id": "music",
                    "rel_path": "album-b/01.flac",
                    "managed_updates": {"title": ["Retagged Title"]},
                }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["preview_token"].as_str().expect("token").to_owned();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &headers,
            Some(json!({"preview_token": token})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(read_title(&file), "Retagged Title");
    // The retag also wrote the approved release's tags.
    let written = droppedneedle_library::tags::read_fields(&file).expect("fields read");
    let release_field = droppedneedle_library::tags::TagField::MusicBrainzReleaseId;
    assert_eq!(written[&release_field], vec![REL_1.to_owned()]);

    // Baseline restore returns the pre-management tags.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/baseline/restore",
            &headers,
            Some(json!({"track_ids": [track_id]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restored"], json!([track_id]));
    assert_eq!(read_title(&file), original_title);
    assert_eq!(
        droppedneedle_library::tags::read_fields(&file).expect("fields read"),
        fields_before,
        "every field the retag wrote is back"
    );
    rig.stop().await;
}

/// The settings page shows how safely each library folder can be written
/// to. Reading it never probes: a folder nothing has written to stays
/// "unknown" and untouched, and one whose probe found no safe method
/// reads "check". Only "Check again" probes, and it cleans up after
/// itself.
#[tokio::test]
async fn library_management_placement_read_never_probes() {
    let rig = Rig::start("lib-placement").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    app.add_library_folder("music", &music).await;

    let placement = "/api/v3/settings/library-management/placement";
    let read = || async {
        let (status, body) = app.call("GET", placement, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };
    let untouched = || std::fs::read_dir(&music).expect("music lists").count() == 0;

    assert_eq!(
        read().await,
        json!({"roots": [{"root_id": "music", "mode": "unknown"}]})
    );
    assert!(untouched(), "a read wrote into the library folder");

    let (status, body) = app
        .call("POST", &format!("{placement}/music/check"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["root_id"], "music");
    let mode = body["mode"].as_str().expect("mode").to_owned();
    assert!(matches!(mode.as_str(), "rename" | "link"), "{body}");
    assert_eq!(body["answered"], true, "{body}");
    assert_eq!(read().await["roots"][0]["mode"], mode.as_str());
    assert!(untouched(), "the probe left something behind");

    // A folder excluded in Library settings after the session opened is
    // no longer listed, and checking it writes nothing into it.
    let set_policy = |policy: &'static str| async move {
        let library = "/api/v3/settings/library";
        let (status, mut settings) = app.call("GET", library, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{settings}");
        let revision = settings["policy_revision"].clone();
        settings["library_roots"][0]["policy"] = json!(policy);
        let (status, body) = app
            .call(
                "PUT",
                library,
                &headers,
                Some(json!({"settings": settings, "expected_policy_revision": revision})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    };
    let check = || async {
        app.call("POST", &format!("{placement}/music/check"), &headers, None)
            .await
    };
    set_policy("excluded").await;
    let (status, body) = check().await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(read().await, json!({"roots": []}));

    // Included again, it reads "unknown". A probe that cannot answer (its
    // folder holds someone else's file) keeps what was known and says so.
    set_policy("automatic").await;
    assert_eq!(read().await["roots"][0]["mode"], "unknown");
    assert!(untouched(), "an excluded folder was written to");
    let probe_dir = music.join(".droppedneedle-management-probe");
    std::fs::create_dir(&probe_dir).expect("probe folder planted");
    std::fs::write(probe_dir.join("stray"), b"x").expect("stray file planted");
    let (status, body) = check().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"root_id": "music", "mode": "unknown", "answered": false})
    );
    assert_eq!(read().await["roots"][0]["mode"], "unknown");
    assert!(
        probe_dir.join("stray").exists(),
        "the stray file was removed"
    );

    let (status, _) = app
        .call(
            "POST",
            &format!("{placement}/elsewhere/check"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    rig.stop().await;
}

/// A retag preview lists, per file, the tags the write would change and
/// nothing else: a file whose album and date differ from the chosen
/// edition lists those two and its MusicBrainz ids; once written, the
/// same file lists no changes.
#[tokio::test]
async fn retag_preview_lists_the_tags_that_change() {
    let rig = Rig::start("lib-manage-changes").await;
    rig.identify.recall_from(|facts| {
        let mut recall = ambiguous_from(facts);
        let artist = |name: &str, join: &str| CreditedArtist {
            id: format!("{name}-id"),
            name: name.to_owned(),
            sort_name: None,
            join: join.to_owned(),
        };
        for release in &mut recall.releases {
            release.title = "Management Album (Deluxe)".to_owned();
            release.date = Some("2025".to_owned());
            for track in &mut release.tracks {
                track.artists = vec![artist("Alpha", " feat. "), artist("Beta", "")];
            }
        }
        recall
    });
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album/01.flac", "management_full.flac");
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    rig.identified(&album, 1).await;
    rig.choose_first_edition(&album).await;
    rig.edition_settled(&album).await;

    let preview = json!({
        "kind": "retag",
        "album_id": album,
        "items": [{"root_id": "music", "rel_path": "album/01.flac", "managed_updates": {}}],
    });
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(preview.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let changes = &body["files"][0]["changes"];
    let fields: Vec<&str> = changes
        .as_array()
        .expect("changes listed")
        .iter()
        .map(|change| change["field"].as_str().expect("field"))
        .collect();
    assert_eq!(fields, ["album", "date", "musicbrainz_ids"], "{body}");
    assert_eq!(
        changes[0],
        json!({"field": "album", "from": "Management Album", "to": "Management Album (Deluxe)"})
    );
    assert_eq!(
        changes[1],
        json!({"field": "date", "from": "2024-03-02", "to": "2025"})
    );

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &headers,
            Some(json!({"preview_token": body["preview_token"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(preview),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["files"][0]["changes"], json!([]), "{body}");
    rig.stop().await;
}

/// An artist whose files carry its MusicBrainz id keeps its row when a
/// managed run renames it in the tags: the credits, the album's artist,
/// a listener's favorite and plays and the MusicBrainz link stay on the
/// row, which takes the new name and sort name. A rescan with nothing new
/// changes no artist row; undoing the run gives the row its old name back.
#[tokio::test]
async fn an_artist_renamed_by_a_managed_run_keeps_its_row() {
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    use droppedneedle_library::tags::{TagField, read_fields};

    const ARTIST: &str = "10000000-0000-4000-8000-000000000005";
    let rig = Rig::start("lib-artist-rename").await;
    rig.identify.recall_from(|facts| {
        let mut recall = ambiguous_from(facts);
        let alfa = CreditedArtist {
            id: ARTIST.to_owned(),
            name: "Alfa".to_owned(),
            sort_name: Some("Alfa, The".to_owned()),
            join: String::new(),
        };
        for release in &mut recall.releases {
            release.artists = vec![alfa.clone()];
            for track in &mut release.tracks {
                track.artists = vec![alfa.clone()];
            }
        }
        recall
    });
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    for number in [1, 2] {
        let relative = format!("album/0{number}.flac");
        let file = plant(&music, &relative, "management_full.flac");
        let one = |value: &str| vec![value.to_owned()];
        let edits = vec![
            TagEdit::verbatim(TagField::Title, one(&format!("Song {number}"))),
            TagEdit::verbatim(TagField::TrackNumber, one(&number.to_string())),
            TagEdit::verbatim(TagField::Compilation, Vec::new()),
            TagEdit::verbatim(TagField::Artist, one("Alpha")),
            TagEdit::verbatim(TagField::Artists, one("Alpha")),
            TagEdit::verbatim(TagField::ArtistSort, Vec::new()),
            TagEdit::verbatim(TagField::AlbumArtist, one("Alpha")),
            TagEdit::verbatim(TagField::AlbumArtists, one("Alpha")),
            TagEdit::verbatim(TagField::AlbumArtistSort, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzArtistId, one(ARTIST)),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, one(ARTIST)),
            TagEdit::verbatim(
                TagField::MusicBrainzRecordingId,
                one(&format!("10000000-0000-4000-8000-00000000010{number}")),
            ),
            TagEdit::verbatim(
                TagField::MusicBrainzReleaseTrackId,
                one(&format!("10000000-0000-4000-8000-00000000020{number}")),
            ),
        ];
        save_tags(&file, &edits).expect("fixture tagged");
    }
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    rig.identified(&album, 1).await;
    rig.choose_first_edition(&album).await;
    rig.edition_settled(&album).await;

    let db = app.db();
    let text = |sql: &str| -> Vec<String> {
        let mut statement = db.prepare(sql).expect("query prepares");
        statement
            .query_map([], |row| row.get(0))
            .expect("query runs")
            .collect::<Result<_, _>>()
            .expect("rows read")
    };
    let rows_of_album = format!(
        "SELECT c.local_artist_id FROM local_track_artists c JOIN local_tracks t \
         ON t.id = c.local_track_id WHERE t.local_album_id = '{album}' \
         UNION ALL SELECT local_artist_id FROM local_album_artists WHERE local_album_id = '{album}' \
         UNION ALL SELECT album_artist_id FROM local_albums WHERE id = '{album}'"
    );
    let artist = text(&rows_of_album)[0].clone();
    assert_eq!(text(&rows_of_album), vec![artist.clone(); 4]);
    let user = text("SELECT id FROM auth_users WHERE role = 'admin'")[0].clone();
    // The app linked the artist from its files' id by now; v2's link of
    // the same id takes its place.
    db.execute_batch(&format!(
        "INSERT OR REPLACE INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('{artist}', 'musicbrainz', '{ARTIST}', 'legacy_import', 1); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('{user}', 'artist', '{artist}', 1); \
         INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
         artist_name, played_at) VALUES ('play-1', '{user}', '{artist}', 'Song 1', 'Alpha', \
         '2026-01-01T00:00:00Z');"
    ))
    .expect("link, favorite and play seeded");

    let retag =
        |rel_path: &str| json!({"root_id": "music", "rel_path": rel_path, "managed_updates": {}});
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(json!({
                "kind": "retag",
                "album_id": album,
                "items": [retag("album/01.flac"), retag("album/02.flac")],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &headers,
            Some(json!({"preview_token": body["preview_token"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle = body["bundle_id"].as_str().expect("bundle").to_owned();
    let written = read_fields(&music.join("album/02.flac")).expect("fields read");
    assert_eq!(
        (
            &written[&TagField::Artists],
            &written[&TagField::ArtistSort]
        ),
        (&vec!["Alfa".to_owned()], &vec!["Alfa, The".to_owned()]),
        "the run renamed the artist in the tags"
    );
    app.scan_library().await;

    assert_eq!(text(&rows_of_album), vec![artist.clone(); 4]);
    let named = format!(
        "SELECT display_name || ' / ' || sort_name FROM local_artists WHERE id = '{artist}'"
    );
    assert_eq!(text(&named), ["Alfa / Alfa, The"]);
    assert_eq!(
        text(
            "SELECT item_id FROM library_user_favorites WHERE item_kind = 'artist' \
             UNION ALL SELECT local_artist_id FROM library_play_history"
        ),
        vec![artist.clone(); 2]
    );
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/artists/{artist}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["artist_mbid"], json!(ARTIST), "{body}");
    assert_eq!(
        text(&format!(
            "SELECT id FROM local_artists WHERE display_name = 'Alfa' \
             AND retired_into_artist_id IS NULL AND id <> '{artist}'"
        )),
        Vec::<String>::new()
    );

    let revisions = "SELECT group_concat(id || ':' || row_revision) FROM \
                     (SELECT id, row_revision FROM local_artists ORDER BY id)";
    let before = text(revisions);
    app.scan_library().await;
    assert_eq!(text(revisions), before, "a rescan with nothing new");

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/undo",
            &headers,
            Some(json!({"bundle_id": bundle})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.scan_library().await;
    assert_eq!(text(&rows_of_album), vec![artist.clone(); 4]);
    assert_eq!(
        text(&format!(
            "SELECT display_name FROM local_artists WHERE id = '{artist}'"
        )),
        ["Alpha"]
    );
    rig.stop().await;
}

/// A trusted user may write an album's edition tags into its files and
/// undo that write, nothing more: no destination, no tag values of their
/// own, no undo of anyone else's write and no restore. An admin's same
/// calls go through.
#[tokio::test]
async fn manage_routes_limit_a_trusted_user_to_a_plain_retag() {
    use droppedneedle_library::tags::{TagField, read_fields};

    let rig = Rig::start("lib-manage-roles").await;
    rig.identify.recall_from(ambiguous_from);
    let app = &rig.app;
    let admin_auth = app.login_admin().await;
    let admin_headers = [("authorization", admin_auth.as_str())];
    let mut curators = Vec::new();
    for name in ["tris", "kit"] {
        let login = app.login_user(name).await;
        let (status, body) = app
            .call(
                "PUT",
                &format!("/api/v3/admin/users/{}/role", login.user_id),
                &admin_headers,
                Some(json!({"role": "trusted"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        curators.push(login.authorization);
    }
    let tris_headers = [("authorization", curators[0].as_str())];
    let kit_headers = [("authorization", curators[1].as_str())];
    let molly = app.login_user("molly").await;
    let molly_headers = [("authorization", molly.authorization.as_str())];

    let music = app.config.root_app_dir.join("music");
    let file = plant(&music, "album/01.flac", "management_full.flac");
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    rig.identified(&album, 1).await;
    rig.choose_first_edition(&album).await;
    let fields_before = read_fields(&file).expect("fields read");

    // A second identified album: its file is never part of a preview
    // for the first, whoever asks.
    plant(&music, "other/01.flac", "management_full.flac");
    app.scan_library().await;
    let other_track = rig
        .scan_store
        .track_at("music", "other/01.flac")
        .unwrap()
        .expect("track indexed");
    let other_album = rig
        .scan_store
        .album_for_track(&other_track)
        .unwrap()
        .expect("album");
    assert_ne!(other_album, album);
    rig.identified(&other_album, 1).await;
    rig.choose_first_edition(&other_album).await;
    for headers in [&tris_headers, &admin_headers] {
        let (status, body) = app
            .call(
                "POST",
                "/api/v3/library/manage/preview",
                headers,
                Some(json!({
                    "kind": "retag",
                    "album_id": album,
                    "items": [{
                        "root_id": "music",
                        "rel_path": "other/01.flac",
                        "managed_updates": {},
                    }],
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    let preview = |dest_rel: Option<&str>, updates: Value| {
        let mut item = json!({
            "root_id": "music",
            "rel_path": "album/01.flac",
            "managed_updates": updates,
        });
        if let Some(dest_rel) = dest_rel {
            item["dest_rel"] = json!(dest_rel);
        }
        json!({
            "kind": if dest_rel.is_some() { "organize" } else { "retag" },
            "album_id": album,
            "items": [item],
        })
    };
    let with_destination = preview(Some("elsewhere/01.flac"), json!({}));
    let with_updates = preview(None, json!({"title": ["Their Title"]}));

    for refused in [&with_destination, &with_updates] {
        let (status, body) = app
            .call(
                "POST",
                "/api/v3/library/manage/preview",
                &tris_headers,
                Some(refused.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}: {body}");
        assert_eq!(body["error"]["code"], json!("FORBIDDEN"));
    }
    assert_eq!(read_fields(&file).expect("fields read"), fields_before);

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &tris_headers,
            Some(preview(None, json!({}))),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["preview_token"].as_str().expect("token").to_owned();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &tris_headers,
            Some(json!({"preview_token": token})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bundle_id = body["bundle_id"].as_str().expect("bundle").to_owned();
    assert_eq!(
        read_fields(&file).expect("fields read")[&TagField::MusicBrainzReleaseId],
        vec![REL_1.to_owned()],
        "the plain retag wrote the edition's tags"
    );

    // Only the curator who wrote the tags, or an admin, may undo them.
    // Another curator is told the write does not exist; a plain user is
    // refused outright.
    let undo = json!({"bundle_id": bundle_id});
    let undo_uri = "/api/v3/library/manage/undo";
    let (status, body) = app
        .call("POST", undo_uri, &kit_headers, Some(undo.clone()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = app
        .call("POST", undo_uri, &molly_headers, Some(undo.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = app.call("POST", undo_uri, &tris_headers, Some(undo)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(read_fields(&file).expect("fields read"), fields_before);

    let restore = json!({"track_ids": [track]});
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/baseline/restore",
            &tris_headers,
            Some(restore.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(file.is_file(), "the retagged file stays where it was");

    // An admin may undo a curator's write.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &tris_headers,
            Some(preview(None, json!({}))),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &tris_headers,
            Some(json!({"preview_token": body["preview_token"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let undo = json!({"bundle_id": body["bundle_id"]});

    for (uri, request) in [
        ("/api/v3/library/manage/preview", &with_destination),
        ("/api/v3/library/manage/preview", &with_updates),
        ("/api/v3/library/manage/undo", &undo),
        ("/api/v3/library/manage/baseline/restore", &restore),
    ] {
        let (status, body) = app
            .call("POST", uri, &admin_headers, Some(request.clone()))
            .await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }

    // An admin's move preview is not the trusted user's to apply, and
    // their refused attempt leaves it for the admin.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &admin_headers,
            Some(with_destination.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let apply = json!({"preview_token": body["preview_token"]});
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &tris_headers,
            Some(apply.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let moved = music.join("elsewhere/01.flac");
    assert!(file.is_file() && !moved.exists(), "nothing moved");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &admin_headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(moved.is_file() && !file.exists(), "the admin's move lands");
    rig.stop().await;
}

/// Read an edition's tag write until it is planned, written or failed.
async fn settled_tag_write(app: &TestApp, headers: &[(&str, &str)], uri: &str) -> Value {
    app.until("the tag write settles", || async move {
        let (status, body) = app.call("GET", uri, headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        matches!(body["step"].as_str(), Some("ready" | "done" | "failed")).then_some(body)
    })
    .await
}

/// Apply a planned tag write as `headers` and wait until it is written.
async fn write_planned(app: &TestApp, headers: &[(&str, &str)], uri: &str, token: &Value) {
    let planned = settled_tag_write(app, headers, uri).await;
    assert_eq!(planned["step"], json!("ready"), "{planned}");
    assert_eq!(planned["ready_to_apply"], json!(true), "{planned}");
    let (status, body) = app
        .call(
            "POST",
            &format!("{uri}/apply"),
            headers,
            Some(json!({
                "preview_token": token,
                "expected_row_revision": planned["row_revision"],
                "idempotency_key": format!("apply-{uri}"),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let written = settled_tag_write(app, headers, uri).await;
    assert_eq!(written["step"], json!("done"), "{written}");
}

/// Plan the undo of a written tag write as `headers`, apply it, and wait
/// until the files are back.
async fn undo_written(app: &TestApp, headers: &[(&str, &str)], tags: &str, job: &str) {
    let uri = format!("{tags}/{job}");
    let written = settled_tag_write(app, headers, &uri).await;
    let (status, body) = app
        .call(
            "POST",
            &format!("{uri}/undo"),
            headers,
            Some(json!({
                "expected_row_revision": written["row_revision"],
                "idempotency_key": format!("undo-{job}"),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let undo = format!("{tags}/{}", body["job_id"].as_str().expect("undo job"));
    let planned = settled_tag_write(app, headers, &undo).await;
    assert_eq!(planned["undo"], json!(true), "{planned}");
    assert!(
        planned["files"]
            .as_array()
            .expect("files")
            .iter()
            .all(|file| file["blocked"].is_null()),
        "{planned}"
    );
    write_planned(app, headers, &undo, &body["preview_token"]).await;
}

/// "Write tags" on the edition sheet, by a trusted user in a folder the
/// app does not write by itself: the preview lists what changes, and the
/// write puts in what the folder's profile writes (its tagging script, the
/// folder's own override, a file's saved override) without renaming
/// anything, though the profile renames. The writer's undo puts every tag
/// back and leaves no record a wider preview would read as a change made
/// outside the app. Another trusted user is told none of it exists; an
/// admin reaches all of it. A held album, a custom edition and an album
/// still being placed are refused before any job is made.
#[tokio::test]
async fn an_editions_tags_are_written_as_the_folders_profile_says() {
    use droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID as PICARD;
    use droppedneedle_library::tags::save::read_document;
    use droppedneedle_library::tags::{TagField, read_fields};
    const SCRIPT: &str = "33333333-3333-4333-8333-333333333333";

    let rig = Rig::start("lib-edition-tags").await;
    let app = &rig.app;
    let admin_auth = app.login_admin().await;
    let admin = [("authorization", admin_auth.as_str())];
    let mut curators = Vec::new();
    for name in ["tris", "kit"] {
        let login = app.login_user(name).await;
        let (status, body) = app
            .call(
                "PUT",
                &format!("/api/v3/admin/users/{}/role", login.user_id),
                &admin,
                Some(json!({"role": "trusted"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        curators.push(login.authorization);
    }
    let tris = [("authorization", curators[0].as_str())];
    let kit = [("authorization", curators[1].as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, _) = identified_album(&rig, &music, "album-e").await;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let tags = format!("{edition}/tags");
    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &tris,
            Some(json!({"release_mbid": fit.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.edition_settled(&album).await;
    let files = [music.join("album-e/01.flac"), music.join("album-e/02.flac")];
    let originals: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();
    // Every tag frame and picture of each file, as the file holds them.
    let tags_now = || {
        files
            .iter()
            .map(|file| {
                let document = read_document(file).expect("tags read");
                let pictures: Vec<Vec<u8>> = document
                    .pictures
                    .iter()
                    .map(|picture| picture.data.clone())
                    .collect();
                (document.native, pictures)
            })
            .collect::<Vec<_>>()
    };
    let original_tags = tags_now();
    let db = app.db();
    let track = rig
        .scan_store
        .track_at("music", "album-e/01.flac")
        .unwrap()
        .expect("track indexed");

    // The profile renames and has its tags off; the folder turns them on
    // and runs a script; one file keeps a title saved for it.
    let page = "/api/v3/settings/library-management";
    let (status, saved) = app.call("GET", page, &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let mut settings = saved.clone();
    settings
        .as_object_mut()
        .expect("settings")
        .remove("settings_revision");
    settings["tagging_scripts"]
        .as_array_mut()
        .expect("scripts")
        .push(json!({
            "id": SCRIPT,
            "name": "Comment",
            "source": "set comment = \"Tagged by the folder\"",
        }));
    for profile in settings["profiles"].as_array_mut().expect("profiles") {
        if profile["id"] == json!(PICARD) {
            assert_eq!(profile["organization"]["rename_enabled"], json!(true));
            profile["metadata"]["enabled"] = json!(false);
            profile["metadata"]["tagging_script_ids"] = json!([SCRIPT]);
        }
    }
    settings["root_assignments"] = json!([{
        "root_id": "music",
        "profile_id": PICARD,
        "enabled": false,
        "overrides": {"metadata_enabled": true},
    }]);
    let (status, body) = app
        .call(
            "PUT",
            page,
            &admin,
            Some(json!({
                "settings": settings,
                "expected_settings_revision": saved["settings_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    db.execute(
        "INSERT INTO library_management_overrides (id, subject_kind, local_track_id, \
         field_name, value_json, mode, subject_revision, created_at, updated_at) \
         VALUES ('ov-title', 'track', ?1, 'title', '\"Kept Title\"', 'replace', 1, 1, 1)",
        [&track],
    )
    .expect("override saved");

    // Refused with a reason, and no job made: a stale revision, a held
    // album, one still being placed, and a custom edition.
    let jobs = || -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM library_operation_jobs WHERE kind = 'library_management'",
            [],
            |row| row.get(0),
        )
        .expect("jobs counted")
    };
    let made = jobs();
    let ask =
        |revision: &Value| json!({"release_mbid": fit.id, "expected_identity_revision": revision});
    let refused = |body: Value| {
        let (tags, tris) = (&tags, &tris);
        async move {
            let (status, answer) = app.call("POST", tags, tris, Some(body)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{answer}");
            answer["error"]["code"].clone()
        }
    };
    let revision = edition_status(app, &tris, &edition).await["identity_revision"].clone();
    assert_eq!(refused(ask(&json!(999))).await, json!("STALE_REVISION"));
    exclude_by_hand(&db, &album);
    assert_eq!(refused(ask(&revision)).await, json!("EDITION_TAGS_HELD"));
    db.execute(
        "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
        [&album],
    )
    .expect("hold lifted");
    db.execute(
        "INSERT INTO library_edition_remap_queue \
         (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
         VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
        rusqlite::params![album, fit.id],
    )
    .expect("remap row queued");
    assert_eq!(
        refused(ask(&revision)).await,
        json!("EDITION_REMAP_PENDING")
    );
    db.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        [&album],
    )
    .expect("remap row removed");
    let (status, body) = app
        .call(
            "POST",
            &format!("{edition}/custom"),
            &admin,
            Some(ask(&revision)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        refused(ask(&body["identity_revision"])).await,
        json!("EDITION_TAGS_CUSTOM")
    );
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &admin, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(jobs(), made, "no job for a refusal");
    let revision = body["identity_revision"].clone();

    // The preview lists the changes; nothing is written yet.
    let (status, queued) = app.call("POST", &tags, &tris, Some(ask(&revision))).await;
    assert_eq!(status, StatusCode::OK, "{queued}");
    let job = queued["job_id"].as_str().expect("job id").to_owned();
    let uri = format!("{tags}/{job}");
    let planned = settled_tag_write(app, &tris, &uri).await;
    assert_eq!(planned["step"], json!("ready"), "{planned}");
    let first = planned["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["path"] == json!("album-e/01.flac"))
        .expect("the first file is planned")
        .clone();
    let change = |field: &str| {
        first["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .find(|change| change["field"] == json!(field))
            .map(|change| change["to"].clone())
    };
    assert_eq!(change("title"), Some(json!("Kept Title")), "{first}");
    assert_eq!(
        change("comment"),
        Some(json!("Tagged by the folder")),
        "{first}"
    );
    let unchanged: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();
    assert_eq!(unchanged, originals, "nothing is written by a preview");

    // Another trusted user is told it does not exist; an admin reads it.
    let (status, body) = app.call("GET", &uri, &kit, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = app
        .call(
            "POST",
            &format!("{uri}/apply"),
            &kit,
            Some(json!({
                "preview_token": queued["preview_token"],
                "expected_row_revision": planned["row_revision"],
                "idempotency_key": "kit-apply",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = app.call("GET", &uri, &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The write holds the profile's fields; no file is renamed or moved.
    write_planned(app, &tris, &uri, &queued["preview_token"]).await;
    let fields = read_fields(&files[0]).expect("fields read");
    assert_eq!(fields[&TagField::Title], vec!["Kept Title".to_owned()]);
    assert_eq!(
        fields[&TagField::Comment],
        vec!["Tagged by the folder".to_owned()]
    );
    assert_eq!(
        fields[&TagField::MusicBrainzReleaseId],
        vec![fit.id.clone()]
    );
    let mut names: Vec<String> = std::fs::read_dir(music.join("album-e"))
        .expect("album folder reads")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, ["01.flac", "02.flac"], "nothing renamed or moved");
    assert_ne!(file_sha256(&files[0]), originals[0]);

    let (status, body) = app
        .call(
            "POST",
            &format!("{uri}/undo"),
            &kit,
            Some(json!({"expected_row_revision": 0, "idempotency_key": "kit-undo"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    undo_written(app, &tris, &tags, &job).await;
    assert_eq!(
        tags_now(),
        original_tags,
        "the writer's undo puts every tag back"
    );

    // An admin applies and undoes a trusted user's write.
    let (status, queued) = app.call("POST", &tags, &tris, Some(ask(&revision))).await;
    assert_eq!(status, StatusCode::OK, "{queued}");
    let job = queued["job_id"].as_str().expect("job id").to_owned();
    write_planned(
        app,
        &admin,
        &format!("{tags}/{job}"),
        &queued["preview_token"],
    )
    .await;
    assert_ne!(file_sha256(&files[0]), originals[0]);
    undo_written(app, &admin, &tags, &job).await;
    assert_eq!(
        tags_now(),
        original_tags,
        "the admin's undo puts every tag back"
    );

    // A preview of this album and another does not read the undone write
    // as a change made outside the app.
    rig.identify.recall_from(ambiguous_from);
    plant(&music, "other/01.flac", "management_full.flac");
    app.scan_library().await;
    app.until("identification settles", || {
        let db = &db;
        async move {
            let open: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM library_identify_jobs \
                     WHERE state IN ('queued', 'running')",
                    [],
                    |row| row.get(0),
                )
                .expect("identify queue reads");
            (open == 0).then_some(())
        }
    })
    .await;
    let other_track = rig
        .scan_store
        .track_at("music", "other/01.flac")
        .unwrap()
        .expect("track indexed");
    let other = rig
        .scan_store
        .album_for_track(&other_track)
        .unwrap()
        .expect("album");
    assert_ne!(other, album);
    let (status, management) = app.call("GET", page, &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{management}");
    let (status, library) = app
        .call("GET", "/api/v3/settings/library", &admin, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{library}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/management/previews",
            &admin,
            Some(json!({
                "selection": {"kind": "albums", "ids": [album, other]},
                "profile_id": PICARD,
                "expected_settings_revision": management["settings_revision"],
                "expected_policy_revision": library["policy_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview = format!(
        "/api/v3/library/management/previews/{}",
        body["job_id"].as_str().expect("job id")
    );
    app.until("the wide preview is planned", || {
        let (preview, admin) = (&preview, &admin);
        async move {
            let (status, body) = app.call("GET", preview, admin, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            (body["phase"] == json!("ready")).then_some(())
        }
    })
    .await;
    let (status, body) = app
        .call(
            "GET",
            &format!("{preview}/items?album_id={album}"),
            &admin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "{body}");
    for item in items {
        assert_ne!(item["reason_code"], json!("CHANGED_OUTSIDE"), "{item}");
    }
    rig.stop().await;
}

/// A tag write asked for one edition writes nothing when another edition
/// is chosen before the worker plans it: planning finds the album's
/// identity moved on and blocks every file.
#[tokio::test]
async fn an_edition_tag_write_planned_after_another_choice_writes_nothing() {
    use crate::common::auth::test_principal;
    use droppedneedle_auth::Role;
    use droppedneedle_auth::session::store::SessionKind;
    use droppedneedle_library::management::models::ItemFilter;
    use droppedneedle_library::management::service::ApplyRequest;

    let managed = ManagedLibrary::start("lib-edition-tags-pin", |_| {}).await;
    let (album, tracks) = managed.add_album("pinned", 1).await;
    let file = managed.path(&tracks[0]);
    let before = file_sha256(&file);
    let identity = managed
        .library
        .identify_store
        .album_identity(&album)
        .expect("identity reads")
        .expect("identity");
    let release = identity.release_mbid.clone().expect("an edition");
    let other = if release == REL_1 { REL_2 } else { REL_1 };
    let management = managed.management();
    let handle = management
        .create_edition_tags(
            &album,
            &release,
            i64::try_from(identity.row_revision).expect("revision"),
            &test_principal("admin", Role::Admin, "session-1", SessionKind::Standard),
        )
        .expect("tag write queued");
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, other, Some("admin"))
        .await
        .expect("another edition chosen");

    assert!(management_step(&managed.library).await, "planning runs");
    let page = management
        .items(&handle.job_id, -1, 100, &ItemFilter::default())
        .expect("items page");
    assert!(!page.items.is_empty());
    for item in &page.items {
        assert_eq!(
            item.facts
                .reason
                .as_ref()
                .map(|reason| reason.code.as_str()),
            Some("EDITION_CHANGED"),
            "{item:?}"
        );
    }
    let detail = management.detail(&handle.job_id).expect("detail");
    let applied = management.apply(
        &handle.job_id,
        ApplyRequest {
            preview_token: handle.preview_token,
            expected_row_revision: detail.job.row_revision,
            idempotency_key: "apply-pinned".to_owned(),
            confirmation: true,
        },
    );
    assert!(applied.is_err(), "nothing can be written");
    while management_step(&managed.library).await {}
    assert_eq!(file_sha256(&file), before, "the file is as it was");
}

/// When a person's edition choice answers, the album's artist proofs and
/// links are already written, with no sweep and no identification run.
#[tokio::test]
async fn an_edition_choice_links_its_artists_before_it_answers() {
    use rusqlite::OptionalExtension as _;
    const ARTIST: &str = "dddddddd-0000-4000-8000-00000000000d";
    let (scratch, library, music) = bare_library("lib-choice-links").await;
    plant(&music, "a/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    rescan(&library).await;
    let track = library
        .scan_store
        .track_at("music", "a/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let mut recall = ambiguous_recall(&library.identify_store, &album);
    for release in &mut recall.releases {
        release.artists[0].id = ARTIST.to_owned();
    }
    scratch.identify.set_recall(recall);

    crate::common::library::operations(&library)
        .choose_edition(&album, REL_1, Some("admin"))
        .await
        .expect("edition chosen");

    let db = droppedneedle_kernel::db::open_connection(&scratch.join("app.db")).expect("db opens");
    let credited: String = db
        .query_row(
            "SELECT local_artist_id FROM local_album_artists WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("album credit");
    let proven: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_artist_credit_proofs \
             WHERE subject_kind = 'album' AND subject_id = ?1 AND artist_mbid = ?2",
            [&album, ARTIST],
            |row| row.get(0),
        )
        .expect("proofs read");
    assert_eq!(proven, 1, "the album credit is proven");
    let link: Option<String> = db
        .query_row(
            "SELECT provider_artist_id FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
            [&credited],
            |row| row.get(0),
        )
        .optional()
        .expect("link reads");
    assert_eq!(link.as_deref(), Some(ARTIST), "the album artist is linked");
}

/// Library roots live in the settings, so a restart comes back with the
/// same roots under the same ids, and the next scan finds nothing new.
#[tokio::test]
async fn library_roots_survive_a_restart() {
    let rig = Rig::start("lib-restart").await;
    let music = rig.app.config.root_app_dir.join("music");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;

    let rig = rig.restart().await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let (status, body) = app
        .call("GET", "/api/v3/library/roots", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], json!(true));
    let roots = body["roots"].as_array().expect("roots");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["id"], json!("music"));
    assert_eq!(roots[0]["path"], json!(music.to_string_lossy()));
    // The same directory, or one inside it, cannot become a second root.
    for overlapping in [music.clone(), music.join("album")] {
        let (status, body) = app
            .call(
                "POST",
                "/api/v3/library/roots",
                &headers,
                Some(json!({"path": overlapping.to_string_lossy()})),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "overlapping root refused: {body}"
        );
    }
    let run = app.scan_library().await;
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/scan/runs/{run}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["counters"]["new_count"], json!(0), "{body}");
    assert_eq!(
        body["run"]["counters"]["unchanged_count"],
        json!(1),
        "{body}"
    );
    rig.stop().await;
}

/// Scan controls and the activity feed through the HTTP API: the policy
/// guard, scan kinds, history, estimates, and pausing identification. The
/// app scans as soon as it is asked, so a run is never caught waiting. It
/// clears a finished run's file list on its next pass and keeps the list
/// of files the run could not read.
#[tokio::test]
async fn library_journey_scan_controls_and_activity() {
    let rig = Rig::start("lib-controls").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-a/01.flac", "management_full.flac");
    plant(&music, "album-a/02.flac", "management_full.flac");
    // Not audio at all: its tags cannot be read.
    std::fs::write(music.join("album-a/03.flac"), b"not a flac file").expect("junk file");
    let post = |uri: String, body: Value| async move {
        app.call("POST", &uri, &headers, Some(body)).await
    };
    let get = |uri: String| async move { app.call("GET", &uri, &headers, None).await };

    // Identification is paused before there is anything to identify, so
    // what the scans queue is still waiting when the journey looks.
    let (status, body) = get("/api/v3/library/activity".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let control = control_revision(&body);
    let (status, body) = post(
        "/api/v3/library/identification/pause".into(),
        json!({"expected_revision": control}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("paused"));
    let paused_at = body["row_revision"].as_u64().expect("revision");

    let first_scan = app.add_library_folder("music", &music).await;
    // The revision the settings page shows is the one requests carry.
    let (status, body) = get("/api/v3/settings/library".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let revision = body["policy_revision"]
        .as_str()
        .expect("policy revision")
        .to_owned();

    // Stale settings and unknown scopes are refused before anything queues.
    let (status, _) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "rescan_files", "expected_policy_revision": "stale"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"scope_ids": ["gone"], "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A requested run is accepted and runs.
    let (status, body) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "incremental", "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let queued_id = body["run_id"].as_str().expect("run id").to_owned();
    app.scan_ended(&queued_id).await;
    let (status, body) = get("/api/v3/library/scan/runs/current".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], Value::Null, "{body}");

    // A full rescan runs to the end and lands in history.
    let (status, body) = get("/api/v3/library/scan/runs/estimate".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["estimated_file_count"], json!(2));
    let (status, body) = post(
        "/api/v3/library/scan/runs".into(),
        json!({"kind": "rescan_files", "scope_ids": ["music"], "expected_policy_revision": revision}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run_id = body["run_id"].as_str().expect("run id").to_owned();
    app.scan_ended(&run_id).await;
    let (status, body) = get("/api/v3/library/scan/runs/history?limit=1".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], json!(run_id));
    assert_eq!(body["items"][0]["kind"], json!("rescan_files"));
    assert_eq!(body["items"][0]["state"], json!("completed"));
    let cursor = body["next_cursor"].as_str().expect("older page").to_owned();
    let (status, body) = get(format!(
        "/api/v3/library/scan/runs/history?limit=1&cursor={cursor}"
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], json!(queued_id));
    // The folder's first scan could not read one file.
    let (status, body) = get(format!("/api/v3/library/scan/runs/{first_scan}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["counters"]["errored_count"], json!(1), "{body}");
    // Once the app has cleared the rescan's working file list, the file
    // it could not read is still listed with its code, a plain reason and
    // an action.
    app.until("the finished scan's file list is cleared", || async {
        let (status, body) = get(format!("/api/v3/library/scan/runs/{run_id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["files"].as_array()?.is_empty().then_some(())
    })
    .await;
    let (status, body) = get(format!("/api/v3/library/scan/runs/{run_id}/failures")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let failures = body["items"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "{body}");
    assert_eq!(failures[0]["relative_path"], json!("album-a/03.flac"));
    assert_eq!(failures[0]["failure_code"], json!("TAG_READ_FAILED"));
    assert!(
        failures[0]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("Can't read this file's tags"))
    );
    assert!(
        failures[0]["action"]
            .as_str()
            .is_some_and(|action| !action.is_empty())
    );
    let (status, _) = get("/api/v3/library/scan/runs/nope/failures".into()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = get("/api/v3/library/scan/runs/estimate?scope_ids=music".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["estimated_file_count"], json!(2));

    // The scan queued the album for identification; pausing holds it.
    let (status, body) = get("/api/v3/library/activity".into()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for stream in ["scan", "identification", "operation", "catalog"] {
        assert!(body["revisions"][stream].is_u64(), "{stream} revision");
    }
    let card = identification_card(&body).expect("identification card");
    assert_eq!(card["waiting_count"], json!(1));
    let (status, _) = post(
        "/api/v3/library/identification/resume".into(),
        json!({"expected_revision": control}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "a stale switch is refused");
    let (status, body) = post(
        "/api/v3/library/identification/resume".into(),
        json!({"expected_revision": paused_at}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("running"));
    let track = rig
        .scan_store
        .track_at("music", "album-a/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    rig.identified(&album, 1).await;
    rig.stop().await;
}

/// The identification card of the activity feed, shown while albums wait.
fn identification_card(activity: &Value) -> Option<&Value> {
    activity["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == json!("identification"))
}

/// The revision the identification switch expects: the card's, or that of
/// a switch nobody has touched while there is no card.
fn control_revision(activity: &Value) -> u64 {
    identification_card(activity).map_or(1, |card| {
        card["control_revision"].as_u64().expect("control revision")
    })
}

/// A run whose stored state the server does not know answers 500, never a
/// state it made up.
#[tokio::test]
async fn a_run_in_an_unknown_state_is_not_read_as_another() {
    let app = TestApp::start("lib-unknown-state").await;
    let auth = app.login_admin().await;
    // The database's own check is set aside to store a state no release
    // wrote, as a damaged or foreign file could hold it.
    app.db()
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope,
                 queued_at, updated_at, terminal_at) VALUES
                 ('strange', 'incremental', 'manual', 'resting', 'queued', 'all', 1.0, 1.0, 1.0);",
        )
        .expect("row stored");
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/library/scan/runs/strange",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    app.stop().await;
}

/// A folder added on the settings page is scanned as soon as it is saved,
/// and the scan covers that folder only.
#[tokio::test]
async fn a_folder_added_in_settings_is_scanned_at_once_and_alone() {
    let app = TestApp::start("lib-settings-folder").await;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let first = app.config.root_app_dir.join("first");
    plant(&first, "a/01.flac", "management_full.flac");
    app.add_library_folder("first", &first).await;
    let second = app.config.root_app_dir.join("second");
    plant(&second, "b/01.flac", "management_full.flac");
    let runs = || async {
        let (status, body) = app
            .call("GET", "/api/v3/library/scan/runs", &headers, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ids = |list: &str| -> Vec<String> {
            body[list]
                .as_array()
                .expect("runs")
                .iter()
                .map(|run| run["id"].as_str().expect("id").to_owned())
                .collect()
        };
        [ids("current"), ids("history")].concat()
    };
    let before = runs().await;

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/settings/library/paths",
            &headers,
            Some(json!({"path": second.to_string_lossy()})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let root = body["library_roots"]
        .as_array()
        .expect("roots")
        .iter()
        .find(|root| root["path"] == json!(second.to_string_lossy()))
        .unwrap_or_else(|| panic!("the new folder is saved: {body}"))["id"]
        .as_str()
        .expect("root id")
        .to_owned();
    // The scan is asked for before the route answers.
    let new: Vec<String> = runs()
        .await
        .into_iter()
        .filter(|run| !before.contains(run))
        .collect();
    assert_eq!(new.len(), 1, "one scan for the new folder: {new:?}");
    app.scan_ended(&new[0]).await;
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/scan/runs/{}", new[0]),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let scopes = body["scopes"].as_array().expect("scopes");
    assert_eq!(scopes.len(), 1, "{body}");
    assert_eq!(scopes[0]["root_id"], json!(root), "{body}");
    let after: Vec<String> = runs()
        .await
        .into_iter()
        .filter(|run| !before.contains(run))
        .collect();
    assert_eq!(after, new, "no other scan followed");
    app.stop().await;
}

/// A library test's scratch folder, the MusicBrainz answers its library
/// identifies with, and the outside sources Library Management plans with
/// (none unless the test scripts them). Derefs to the folder.
struct Scratch {
    dir: ScratchDir,
    identify: Arc<FakeProviders>,
    enrichment: Option<Arc<dyn droppedneedle_library::management::enrich::EnrichmentPort>>,
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.dir
    }
}

/// A bare library bundle over a migrated scratch database, with its
/// music folder (not yet a root).
async fn bare_library(tag: &str) -> (Scratch, LibrarySetup, PathBuf) {
    bare_library_with(tag, None).await
}

/// As [`bare_library`], with Library Management planning through
/// `enrichment`.
async fn bare_library_with(
    tag: &str,
    enrichment: Option<Arc<dyn droppedneedle_library::management::enrich::EnrichmentPort>>,
) -> (Scratch, LibrarySetup, PathBuf) {
    let scratch = Scratch {
        dir: ScratchDir::new(tag),
        identify: Arc::new(FakeProviders::default()),
        enrichment,
    };
    let connection = droppedneedle_kernel::db::open_connection(&scratch.join("app.db"))
        .expect("scratch db opens");
    droppedneedle_kernel::schema::apply_migrations_blocking(&connection).expect("migrations apply");
    drop(connection);
    let library = open_library(&scratch).await;
    let music = scratch.join("music");
    (scratch, library, music)
}

/// A library bundle over the database and settings in `scratch`, as a
/// restart would build it.
async fn open_library(scratch: &Scratch) -> LibrarySetup {
    let config = Arc::new(
        ConfigStore::scratch(
            scratch,
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
        )
        .expect("config opens"),
    );
    let db = droppedneedle_kernel::db::Db::open_for_tests(&scratch.join("app.db"))
        .expect("database opens");
    // The database is `app.db`, so the undo folder sits beside it as it
    // does beside a server's database.
    let mut app_config = droppedneedle_kernel::config::AppConfig::with_root(0, scratch);
    app_config.library_db_path = scratch.join("app.db");
    let platform = crate::common::TestPlatform::on(&db, scratch)
        .settings(config)
        .config(app_config)
        .build()
        .await;
    library_with(
        &platform,
        scratch.identify.clone(),
        scratch.enrichment.clone(),
    )
}

/// Add `music` as the only root and scan it.
async fn add_music_root(library: &LibrarySetup, music: &Path) {
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            droppedneedle_library::scan::EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(library).await;
}

/// Scan every root again and wait for it.
async fn rescan(library: &LibrarySetup) {
    use droppedneedle_library::scan::{ScanKind, ScanRequest, ScanTrigger};
    let registry = library.core.live_registry();
    library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    drain_scans(library).await;
}

/// A file moved by hand keeps its track id on the next scan, so the
/// favorites and play history that name the track follow the file.
#[tokio::test]
async fn moved_file_keeps_its_track() {
    let rig = Rig::start("lib-move").await;
    let music = rig.app.config.root_app_dir.join("music");
    let old = plant(&music, "before/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "before/01.flac")
        .unwrap()
        .expect("track indexed");
    rig.scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('listener', 'track', '{track}', 0); \
             INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
             artist_name, played_at) VALUES ('play-1', 'listener', '{track}', 'Song', \
             'Artist', '2026-01-01T00:00:00Z');"
        ))
        .expect("favorite and play seed");

    let new = music.join("after/01.flac");
    std::fs::create_dir_all(new.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&old, &new).expect("move by hand");
    rig.app.scan_library().await;

    assert_eq!(
        rig.scan_store.track_at("music", "after/01.flac").unwrap(),
        Some(track.clone()),
        "the moved file continues its track"
    );
    let following = rig
        .scan_store
        .db()
        .query_i64(&format!(
            "SELECT (SELECT COUNT(*) FROM library_user_favorites f JOIN local_tracks t \
             ON t.id = f.item_id WHERE t.id = '{track}' AND t.availability = 'indexed') \
             + (SELECT COUNT(*) FROM library_play_history h JOIN local_tracks t \
             ON t.id = h.local_track_id WHERE t.id = '{track}' \
             AND t.relative_path = 'after/01.flac' AND t.availability = 'indexed')"
        ))
        .expect("count reads");
    assert_eq!(following, 2, "favorite and history follow the file");
    assert_eq!(
        rig.scan_store
            .db()
            .query_i64("SELECT COUNT(*) FROM local_tracks")
            .expect("count reads"),
        1,
        "no second row for the moved file"
    );
    rig.stop().await;
}

/// Two copies of one album in different folders stay two albums (the
/// duplicate tools compare them), and moving one copy keeps its own ids.
#[tokio::test]
async fn copies_stay_apart_and_keep_ids_when_moved() {
    let rig = Rig::start("lib-copies").await;
    let music = rig.app.config.root_app_dir.join("music");
    let copy_a = plant(&music, "flac/01.flac", "flac_full_01.flac");
    let copy_b = plant(&music, "lossy/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;
    let track_a = rig
        .scan_store
        .track_at("music", "flac/01.flac")
        .unwrap()
        .expect("copy a indexed");
    let track_b = rig
        .scan_store
        .track_at("music", "lossy/01.flac")
        .unwrap()
        .expect("copy b indexed");
    let album_a = rig
        .scan_store
        .album_for_track(&track_a)
        .unwrap()
        .expect("album a");
    let album_b = rig
        .scan_store
        .album_for_track(&track_b)
        .unwrap()
        .expect("album b");
    assert_ne!(album_a, album_b, "copies are separate albums");

    let moved = music.join("archive/01.flac");
    std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&copy_b, &moved).expect("move one copy");
    rig.app.scan_library().await;

    assert_eq!(
        rig.scan_store.track_at("music", "archive/01.flac").unwrap(),
        Some(track_b.clone()),
        "the moved copy keeps its track"
    );
    assert_eq!(
        rig.scan_store.album_for_track(&track_b).unwrap(),
        Some(album_b)
    );
    assert_eq!(
        rig.scan_store.album_for_track(&track_a).unwrap(),
        Some(album_a)
    );
    assert!(copy_a.is_file());
    rig.stop().await;
}

/// An album whose files went away in an earlier scan is not handed to a
/// new album that happens to share its names: that one is another
/// release, and the old identity stays where it was.
#[tokio::test]
async fn long_missing_album_is_not_taken_over() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let rig = Rig::start("lib-no-takeover").await;
    let music = rig.app.config.root_app_dir.join("music");
    let green = plant(&music, "green/01.flac", "flac_full_02.flac");
    // Something stays, so the walk is not an empty mount point.
    plant(&music, "keep/01.flac", "flac_no_tags.flac");
    rig.app.add_library_folder("music", &music).await;
    let old_track = rig
        .scan_store
        .track_at("music", "green/01.flac")
        .unwrap()
        .expect("track indexed");
    let old_album = rig
        .scan_store
        .album_for_track(&old_track)
        .unwrap()
        .expect("album");
    std::fs::remove_file(&green).expect("delete");
    rig.app.scan_library().await;
    assert_eq!(
        rig.scan_store.track_at("music", "green/01.flac").unwrap(),
        None
    );

    // Another release under the same names: other songs, no shared ids.
    let red = plant(&music, "red/01.flac", "flac_compilation_01.flac");
    save_tags(
        &red,
        &[
            TagEdit::new(TagField::Album, vec!["OK Computer".to_owned()]),
            TagEdit::new(TagField::AlbumArtist, vec!["Radiohead".to_owned()]),
        ],
    )
    .expect("renamed");
    rig.app.scan_library().await;

    let new_track = rig
        .scan_store
        .track_at("music", "red/01.flac")
        .unwrap()
        .expect("new track indexed");
    assert_ne!(new_track, old_track);
    let new_album = rig
        .scan_store
        .album_for_track(&new_track)
        .unwrap()
        .expect("album");
    assert_ne!(new_album, old_album, "a long-gone album is not reused");
    rig.stop().await;
}

/// A share that comes up empty (unmounted) while a copy of one of its
/// files appears in another root: the guard holds the share back, so its
/// tracks keep their ids and stay available, and the copy gets its own row.
/// A small root that comes up empty is held back the same way.
#[tokio::test]
async fn unmounted_share_keeps_its_tracks() {
    let rig = Rig::start("lib-share-guard").await;
    let dir = &rig.app.config.root_app_dir;
    let local = dir.join("music");
    let share = dir.join("share");
    let shared = plant(&share, "album/01.flac", "flac_full_01.flac");
    // Enough tracks for the mass-missing guard to apply.
    for n in 2..=20 {
        plant(&share, &format!("other/{n:02}.flac"), "flac_no_tags.flac");
    }
    let small = dir.join("small");
    for n in 1..=3 {
        plant(&small, &format!("{n:02}.flac"), "flac_no_tags.flac");
    }
    std::fs::create_dir_all(&local).expect("local root");
    for (id, path) in [("share", &share), ("local", &local), ("small", &small)] {
        rig.app.add_library_folder(id, path).await;
    }
    let track = rig
        .scan_store
        .track_at("share", "album/01.flac")
        .unwrap()
        .expect("share track indexed");

    // The share goes dark; a copy of one of its files lands locally.
    for root in [&share, &small] {
        std::fs::remove_dir_all(root).expect("unmount");
        std::fs::create_dir_all(root).expect("empty mount point");
    }
    plant(&local, "album/01.flac", "flac_full_01.flac");
    assert!(!shared.exists());
    rig.app.scan_library().await;

    assert_eq!(
        rig.scan_store.track_at("share", "album/01.flac").unwrap(),
        Some(track.clone()),
        "the share's track keeps its id and stays available"
    );
    let copy = rig
        .scan_store
        .track_at("local", "album/01.flac")
        .unwrap()
        .expect("copy indexed");
    assert_ne!(copy, track, "the copy gets its own row");
    assert!(
        rig.scan_store
            .track_at("small", "01.flac")
            .unwrap()
            .is_some(),
        "an emptied small root keeps its tracks"
    );
    assert_eq!(
        rig.scan_store
            .db()
            .query_i64("SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing'")
            .expect("count reads"),
        0,
        "nothing is marked missing"
    );
    rig.stop().await;
}

/// Retagging every track of an album to a new name keeps the album row,
/// and with it the curator's identity.
#[tokio::test]
async fn retagged_album_keeps_its_identity() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let rig = Rig::start("lib-retag-album").await;
    let music = rig.app.config.root_app_dir.join("music");
    // No release MBID here: the album is known by its tagged names.
    let files = [
        plant(&music, "album/01.flac", "flac_full_02.flac"),
        plant(&music, "album/02.flac", "flac_full_02.flac"),
    ];
    rig.app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    rig.scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-1', 'manual', 0);"
        ))
        .expect("identity seed");

    for file in &files {
        save_tags(
            file,
            &[TagEdit::new(TagField::Album, vec!["Renamed".to_owned()])],
        )
        .expect("album retagged");
    }
    rig.app.scan_library().await;

    assert_eq!(
        rig.scan_store.album_for_track(&track).unwrap().as_deref(),
        Some(album.as_str()),
        "the retagged album keeps its row"
    );
    assert_eq!(
        rig.scan_store
            .db()
            .query_i64(&format!(
                "SELECT COUNT(*) FROM local_albums WHERE id = '{album}' AND title = 'Renamed'"
            ))
            .expect("count reads"),
        1,
        "the kept row takes the new name"
    );
    assert_eq!(
        rig.scan_store
            .db()
            .query_i64("SELECT COUNT(*) FROM local_albums")
            .expect("count reads"),
        1,
        "no second album"
    );
    rig.stop().await;
}

/// An automatic identity write never replaces a person's album or track
/// row, even when the caller's own check for one was skipped or failed.
#[tokio::test]
async fn automatic_write_keeps_a_manual_identity() {
    use droppedneedle_library::identify::models::{AlbumIdentity, DecisionSource, TrackIdentity};

    let (_scratch, library, music) = bare_library("lib-manual-identity").await;
    plant(&music, "album/01.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    library
        .scan_store
        .db()
        .run_sql(&format!(
            "DELETE FROM local_album_external_identities; \
             DELETE FROM local_track_external_identities; \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
             VALUES ('{album}', 'musicbrainz', 'rg-chosen', 'rel-chosen', 'manual', 0, 3); \
             INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, decision_source, selected_at, row_revision) \
             VALUES ('{track}', 'musicbrainz', 'rec-chosen', 'legacy_import', 0, 3);"
        ))
        .expect("identity seed");

    let store = &library.identify_store;
    store
        .save_album_identity(AlbumIdentity {
            local_album_id: album.clone(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some("rg-download".to_owned()),
            release_mbid: Some("rel-download".to_owned()),
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();
    store
        .save_track_identity(TrackIdentity {
            local_track_id: track.clone(),
            provider: "musicbrainz".to_owned(),
            recording_mbid: Some("rec-download".to_owned()),
            release_track_mbid: None,
            decision_source: DecisionSource::Automatic,
            row_revision: 1,
        })
        .unwrap();

    let kept = store
        .album_identity(&album)
        .unwrap()
        .expect("album identity");
    assert_eq!(kept.decision_source, DecisionSource::Manual);
    assert_eq!(kept.release_group_mbid.as_deref(), Some("rg-chosen"));
    assert_eq!(kept.release_mbid.as_deref(), Some("rel-chosen"));
    assert_eq!(kept.row_revision, 3);
    let kept = store
        .track_identity(&track)
        .unwrap()
        .expect("track identity");
    assert_eq!(kept.decision_source, DecisionSource::LegacyImport);
    assert_eq!(kept.recording_mbid.as_deref(), Some("rec-chosen"));
    assert_eq!(kept.row_revision, 3);

    // A row no person decided stays revisable.
    library
        .scan_store
        .db()
        .run_sql("UPDATE local_track_external_identities SET decision_source = 'embedded';")
        .expect("source flipped");
    store
        .save_track_identity(TrackIdentity {
            local_track_id: track.clone(),
            provider: "musicbrainz".to_owned(),
            recording_mbid: Some("rec-download".to_owned()),
            release_track_mbid: None,
            decision_source: DecisionSource::Automatic,
            row_revision: 4,
        })
        .unwrap();
    let revised = store
        .track_identity(&track)
        .unwrap()
        .expect("track identity");
    assert_eq!(revised.recording_mbid.as_deref(), Some("rec-download"));
}

/// An approval that cannot seal its identity fails as a whole: the
/// caller gets the error and the review stays pending.
#[tokio::test]
async fn failed_approval_leaves_the_review_pending() {
    use droppedneedle_library::service::ServiceError;

    let (scratch, library, music) = bare_library("lib-approve-fail").await;
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    file_edition_review(&library.identify_store, &album);
    let review = library.identification.pending_reviews(&album).unwrap();
    assert_eq!(review.len(), 1);
    library
        .scan_store
        .db()
        .run_sql(
            "CREATE TRIGGER refuse_identity BEFORE INSERT \
             ON local_album_external_identities BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .expect("trigger installs");

    let refused =
        library
            .identification
            .approve_review(&review[0].id, "curator", &format!("rg-1:{REL_1}"));
    assert!(
        matches!(refused, Err(ServiceError::Internal { .. })),
        "a store failure is an error, not an approval"
    );
    assert_eq!(
        library
            .identification
            .pending_reviews(&album)
            .unwrap()
            .len(),
        1,
        "review stays pending"
    );
}

/// A publish journal for a root that is gone never stops boot: recovery
/// finishes, leaves the bundle as it is for a later pass (its root may
/// come back), and reports it held.
#[tokio::test]
async fn journal_on_a_removed_root_waits() {
    let (scratch, first, music) = bare_library("lib-journal-gone").await;
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&first, &music).await;
    first
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, source_root, \
             source_rel, dest_root, dest_rel, staged, staged_sha256, state) \
             VALUES ('j-1', 'bundle-1', 'audio', 'removed', 'a/01.flac', 'removed', \
             'b/01.flac', '/nowhere/.staged', 'abc', 'cleanup_pending');",
        )
        .expect("journal seed");
    drop(first);

    let library = open_library(&scratch).await;
    let recovery = crate::common::library::services(&library)
        .run_recovery()
        .await;

    assert_eq!(recovery.publish_recoveries.len(), 1);
    assert!(recovery.publish_recoveries[0].1.starts_with("Deferred"));
    assert_eq!(
        library
            .publishing
            .held_publish_bundles()
            .expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
    assert_eq!(
        library
            .scan_store
            .db()
            .query_i64(
                "SELECT COUNT(*) FROM library_publish_journal \
                 WHERE id = 'j-1' AND state = 'cleanup_pending'"
            )
            .expect("state reads"),
        1,
        "the bundle is left as it was"
    );
    library
        .publishing
        .publish_tick()
        .expect("maintenance retries");
    assert_eq!(
        library
            .publishing
            .held_publish_bundles()
            .expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
}

/// Identification state is durable: a job the scan queued survives a
/// restart, and a curator's approval stays a manual identity after the
/// next one.
#[tokio::test]
async fn identification_survives_restarts() {
    use droppedneedle_library::identify::models::DecisionSource;

    let rig = Rig::start("lib-identify-restart").await;
    rig.identify.recall_from(ambiguous_from);
    // Paused, so the job the scan queues is still waiting at the restart.
    let paused_at = rig.pause_identification().await;
    let music = rig.app.config.root_app_dir.join("music");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");

    // The scan-queued job is still there after a restart.
    let rig = rig.restart().await;
    let waiting: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM library_identify_jobs WHERE state = 'queued' AND attempts = 0",
            [],
            |row| row.get(0),
        )
        .expect("queue reads");
    assert_eq!(waiting, 1, "queued job survives");
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    rig.choose_first_edition(&album).await;

    let rig = rig.restart().await;
    let identity = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("identity survives");
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.release_mbid.as_deref(), Some(REL_1));
    rig.stop().await;
}

/// Identify the album, then have a person choose the first of two tied
/// editions (the automatic pass only keeps a best guess).
async fn approve_first_candidate(library: &LibrarySetup, identify: &FakeProviders, album: &str) {
    identify.set_recall(ambiguous_recall(&library.identify_store, album));
    assert_eq!(library.identification.identify_tick().await, 1);
    choose_first_edition(library, album).await;
}

/// A person chooses `rel-1` as the album's edition.
async fn choose_first_edition(library: &LibrarySetup, album: &str) {
    crate::common::library::operations(library)
        .choose_edition(album, REL_1, Some("curator"))
        .await
        .expect("edition chosen");
}

/// Preview and apply one managed write; returns the bundle id.
fn publish_one(
    library: &LibrarySetup,
    kind: droppedneedle_library::publish::planner::PlanKind,
    album: &str,
    rel_path: &str,
    dest_rel: Option<&str>,
    updates: &[(&str, &str)],
) -> String {
    use droppedneedle_library::manage::PreviewItemInput;
    let sealed = library
        .publishing
        .plan_preview(
            kind,
            album,
            vec![PreviewItemInput {
                root_id: "music".to_owned(),
                rel_path: rel_path.to_owned(),
                dest_rel: dest_rel.map(str::to_owned),
                managed_updates: updates
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), vec![(*value).to_owned()]))
                    .collect(),
            }],
        )
        .expect("preview seals");
    library
        .publishing
        .apply_preview(&sealed.token, None)
        .expect("apply publishes")
        .bundle_id
}

/// Values a new edit could never carry (a `3/12` track number, a
/// free-text date) come back exactly when a retag is undone.
#[tokio::test]
async fn undo_restores_values_no_new_edit_could_write() {
    use droppedneedle_library::publish::planner::PlanKind;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, music) = bare_library("lib-undo-raw").await;
    let file = plant(&music, "album/01.flac", "management_full.flac");
    save_tags(
        &file,
        &[
            TagEdit::verbatim(TagField::TrackNumber, vec!["3/12".to_owned()]),
            TagEdit::verbatim(TagField::Date, vec!["circa 1970".to_owned()]),
        ],
    )
    .expect("odd values planted");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    approve_first_candidate(&library, &scratch.identify, &album).await;
    let before = read_fields(&file).expect("fields read");

    let bundle = publish_one(
        &library,
        PlanKind::SamePath,
        &album,
        "album/01.flac",
        None,
        &[("date", "2001")],
    );
    let retagged = read_fields(&file).expect("fields read");
    assert_eq!(retagged[&TagField::Date], vec!["2001".to_owned()]);
    assert_ne!(retagged[&TagField::TrackNumber], vec!["3/12".to_owned()]);

    crate::common::library::services(&library)
        .management
        .undo_bundle(&bundle, None)
        .expect("undo publishes");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// A backup carries the undo copies with the database: restored into an
/// empty folder, it still undoes a change made before the backup and
/// still restores originals. A copy the backup lost does not stop the
/// restore; it is listed, and only the file that needed it is left out.
#[tokio::test]
async fn a_restored_backup_still_undoes_and_restores_originals() {
    use droppedneedle_kernel::db::undo_dir::{UNDO_DIR_NAME, UndoDir};
    use droppedneedle_kernel::db::{BackupService, CancelFlag};
    use droppedneedle_library::publish::planner::PlanKind;
    use droppedneedle_library::service::ServiceError;
    use droppedneedle_library::tags::read_fields;

    let (scratch, library, music) = bare_library("lib-backup-round-trip").await;
    let first = plant(&music, "album/01.flac", "flac_full_01.flac");
    let second = plant(&music, "album/02.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = |rel: &str| {
        library
            .scan_store
            .track_at("music", rel)
            .unwrap()
            .expect("track indexed")
    };
    let (one, two) = (track("album/01.flac"), track("album/02.flac"));
    let album = library
        .scan_store
        .album_for_track(&one)
        .unwrap()
        .expect("album");
    approve_first_candidate(&library, &scratch.identify, &album).await;
    let original = read_fields(&first).expect("fields read");
    let retag = |rel: &str, title: &str| {
        publish_one(
            &library,
            PlanKind::SamePath,
            &album,
            rel,
            None,
            &[("title", title)],
        )
    };
    retag("album/01.flac", "First pass");
    let first_pass = read_fields(&first).expect("fields read");
    let later = retag("album/01.flac", "Second pass");
    let other = retag("album/02.flac", "Other");
    let other_title = read_fields(&second).expect("fields read");

    let backups = scratch.join("backups");
    let service = BackupService::new(
        &scratch.join("app.db"),
        &backups,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let report = service
        .backup(None, &CancelFlag::never())
        .await
        .expect("backup runs");
    // The pool loses the copy the second file's undo and original need.
    let lost: String = scratch_db(&scratch)
        .query_row(
            "SELECT blob_sha256 FROM library_publish_baselines WHERE track_id = ?1",
            [&two],
            |row| row.get(0),
        )
        .expect("baseline kept");
    let pool = UndoDir::new(backups.join(UNDO_DIR_NAME));
    assert!(pool.remove(&pool.hold_exclusive().unwrap(), &lost).unwrap());

    let restored = Scratch {
        dir: ScratchDir::new("lib-backup-round-trip-restored"),
        identify: scratch.identify.clone(),
        enrichment: None,
    };
    let target = restored.join("data");
    let outcome = service
        .restore(&report.path, &target, false)
        .expect("restore runs");
    assert_eq!(outcome.undo_missing, vec![lost]);
    // Move the restored database and its undo folder to where the
    // library opens them, with the settings beside them.
    std::fs::rename(target.join("library.db"), restored.join("app.db")).unwrap();
    std::fs::rename(target.join(UNDO_DIR_NAME), restored.join(UNDO_DIR_NAME)).unwrap();
    rusqlite::Connection::open(scratch.join("settings.db"))
        .unwrap()
        .backup(
            rusqlite::DatabaseName::Main,
            restored.join("settings.db"),
            None,
        )
        .unwrap();
    let library = open_library(&restored).await;

    crate::common::library::services(&library)
        .management
        .undo_bundle(&later, None)
        .expect("undo of a change made before the backup publishes");
    assert_eq!(read_fields(&first).expect("fields read"), first_pass);
    match crate::common::library::services(&library)
        .management
        .undo_bundle(&other, None)
    {
        Err(ServiceError::Conflict { message }) => {
            assert!(message.contains("copy_missing"), "{message}")
        }
        Err(error) => panic!("an undo with no saved copy failed otherwise: {error:?}"),
        Ok(_) => panic!("an undo with no saved copy went ahead"),
    }
    library
        .publishing
        .baseline_restore(&[one, two])
        .expect("restore of originals publishes");
    assert_eq!(read_fields(&first).expect("fields read"), original);
    assert_eq!(read_fields(&second).expect("fields read"), other_title);
}

/// Baseline restore takes a file back to how it was before its first
/// managed write, removing fields a later write added: organize first
/// (no tags written), retag second, restore last.
#[tokio::test]
async fn baseline_restore_removes_fields_added_later() {
    use droppedneedle_library::publish::planner::PlanKind;
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, music) = bare_library("lib-baseline-added").await;
    let file = plant(&music, "album/01.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    approve_first_candidate(&library, &scratch.identify, &album).await;
    let before = read_fields(&file).expect("fields read");
    assert!(!before.contains_key(&TagField::MusicBrainzReleaseId));

    publish_one(
        &library,
        PlanKind::Move,
        &album,
        "album/01.flac",
        Some("moved/01.flac"),
        &[],
    );
    publish_one(
        &library,
        PlanKind::SamePath,
        &album,
        "moved/01.flac",
        None,
        &[],
    );
    let moved = music.join("moved/01.flac");
    let retagged = read_fields(&moved).expect("fields read");
    assert_eq!(
        retagged[&TagField::MusicBrainzReleaseId],
        vec![REL_1.to_owned()]
    );

    library
        .publishing
        .baseline_restore(std::slice::from_ref(&track))
        .expect("restore publishes");
    assert!(file.is_file(), "the file is back where it started");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// A publish is checked against its own rows, not the catalog counter: a
/// scan that only adds another album leaves a sealed preview good, and a
/// change to the previewed track's own row refuses it before anything is
/// recorded or staged.
#[tokio::test]
async fn a_publish_checks_its_own_rows() {
    use droppedneedle_library::manage::PreviewItemInput;
    use droppedneedle_library::publish::planner::PlanKind;
    use droppedneedle_library::service::ServiceError;
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, music) = bare_library("lib-own-rows").await;
    let file = plant(&music, "album/01.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    approve_first_candidate(&library, &scratch.identify, &album).await;
    let counter = || {
        library
            .db
            .query_i64("SELECT value FROM library_catalog_revision WHERE singleton = 1")
            .expect("counter reads")
    };
    let preview = |date: &str| {
        library
            .publishing
            .plan_preview(
                PlanKind::SamePath,
                &album,
                vec![PreviewItemInput {
                    root_id: "music".to_owned(),
                    rel_path: "album/01.flac".to_owned(),
                    dest_rel: None,
                    managed_updates: [("date".to_owned(), vec![date.to_owned()])].into(),
                }],
            )
            .expect("preview seals")
    };

    // Another album arriving moves the counter, not this preview's rows.
    let sealed = preview("2001");
    plant(&music, "other/01.flac", "flac_full_01.flac");
    rescan(&library).await;
    assert!(
        library
            .scan_store
            .track_at("music", "other/01.flac")
            .unwrap()
            .is_some(),
        "the other album is indexed"
    );
    let after_scan = counter();
    library
        .publishing
        .apply_preview(&sealed.token, None)
        .expect("a scan of another album leaves the preview good");
    assert_eq!(
        read_fields(&file).expect("fields read")[&TagField::Date],
        vec!["2001".to_owned()]
    );
    assert!(counter() > after_scan, "the commit still bumps the counter");

    // A change to the track's own row refuses the preview at the pregate.
    rescan(&library).await;
    let sealed = preview("2002");
    let bytes = std::fs::read(&file).expect("file reads");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .expect("file opens")
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .expect("mtime moves");
    rescan(&library).await;
    match library.publishing.apply_preview(&sealed.token, None) {
        Err(ServiceError::Conflict { message }) => {
            assert_eq!(message, format!("track {track} changed under the preview"));
        }
        Err(other) => panic!("expected a 409, got {other}"),
        Ok(_) => panic!("expected a 409, the preview applied"),
    }
    assert_eq!(
        library
            .db
            .query_i64(&format!(
                "SELECT COUNT(*) FROM library_publish_operations WHERE bundle_id = '{}'",
                sealed.bundle_id
            ))
            .expect("count reads"),
        0,
        "the pregate refuses before the operation is recorded"
    );
    assert_eq!(std::fs::read(&file).expect("file reads"), bytes);
}

/// A release document answers to its merged ids through their own rows,
/// and a refetch by the canonical id (no redirect, so no merged ids)
/// replaces every row and still answers to them.
#[tokio::test]
async fn release_documents_answer_to_merged_ids() {
    let (_scratch, library, _music) = bare_library("lib-merged-release").await;
    let store = &library.identify_store;
    let mut release = Release {
        id: "rel-new".to_owned(),
        title: "Merged".to_owned(),
        old_ids: vec!["REL-OLD".to_owned()],
        ..Release::default()
    };
    store
        .save_release(&release, droppedneedle_kernel::db::Lane::Background)
        .unwrap();
    release.title = "Refetched".to_owned();
    release.old_ids.clear();
    store
        .save_release(&release, droppedneedle_kernel::db::Lane::Background)
        .unwrap();
    for id in ["rel-new", "rel-old"] {
        let found = store.release(id, None).unwrap().expect("document found");
        assert_eq!(found.id, "rel-new");
        assert_eq!(found.title, "Refetched");
    }
    assert!(store.release("rel-other", None).unwrap().is_none());
}

/// Contribution journey through the real routes and the SQLite store:
/// draft, duplicate check, seeded editor, the release editor coming back
/// on the v1 callback path, verification, and the album linked.
#[tokio::test]
async fn library_contribution_seed_callback_verify_links_album() {
    use droppedneedle_library::contrib::models::{
        MusicBrainzVerifiedRelease, MusicBrainzVerifiedTrack,
    };

    const RELEASE: &str = "11111111-1111-4111-8111-111111111111";
    const GROUP: &str = "22222222-2222-4222-8222-222222222222";
    // Same title and artist, a different tracklist.
    const WRONG: &str = "44444444-4444-4444-8444-444444444444";
    const ARTIST_MBID: &str = "33333333-3333-4333-8333-333333333333";

    let rig = Rig::start("lib-contrib").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];

    // MusicBrainz as the verification worker will find it: one release
    // that shares the title but not the tracklist, and the right one.
    let release = |mbid: &str, titles: [&str; 2]| MusicBrainzVerifiedRelease {
        release_mbid: mbid.to_owned(),
        release_group_mbid: GROUP.to_owned(),
        title: "Test Album".to_owned(),
        artist_name: "Test Artist".to_owned(),
        artist_mbid: Some(ARTIST_MBID.to_owned()),
        tracks: titles
            .iter()
            .enumerate()
            .map(|(index, title)| MusicBrainzVerifiedTrack {
                title: (*title).to_owned(),
                position: index as i64 + 1,
                disc_number: 1,
                duration_seconds: Some(200.0),
                recording_mbid: Some(format!("{mbid}-rec-{index}")),
                release_track_mbid: Some(format!("{mbid}-track-{index}")),
            })
            .collect(),
        ..Default::default()
    };
    let providers = &rig.contrib;
    providers.musicbrainz.insert_verification(
        WRONG,
        Ok(Some(release(
            WRONG,
            ["Somebody Else", "Another Tune Entirely"],
        ))),
    );
    providers.musicbrainz.insert_verification(
        RELEASE,
        Ok(Some(release(RELEASE, ["First Song", "Second Song"]))),
    );

    // One indexed two-track album, as a scan leaves it.
    let db = app.db();
    db.execute_batch(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist-1', 'Test Artist', 'test artist', 'person', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_id, year, grouping_source, created_at, updated_at) \
         VALUES ('album-1', 'music', 'k', 'Test Album', 'test album', 'Test Artist', \
         'artist-1', 2024, 'automatic', 1, 1);",
    )
    .expect("album seeds");
    for (id, number, title) in [("t1", 1, "First Song"), ("t2", 2, "Second Song")] {
        db.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, tag_revision, title, \
             title_folded, artist_name, album_title, album_title_folded, track_number, \
             duration_seconds, file_format, ingest_source, imported_at, membership_source) \
             VALUES (?1, 'album-1', 'music', ?2, ?2, ?1, 1, 1, ?1, ?1, ?3, lower(?3), \
             'Test Artist', 'Test Album', 'test album', ?4, 200.0, 'flac', 'scan', 1, \
             'automatic')",
            rusqlite::params![id, format!("album/{id}.flac"), title, number],
        )
        .expect("track seeds");
    }

    let (status, created) = app
        .call(
            "POST",
            "/api/v3/library/albums/album-1/contributions",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["state"], json!("draft"));
    let id = created["id"].as_str().expect("contribution id").to_owned();
    // The album view carries the open contribution for the album page.
    let (status, album) = app
        .call("GET", "/api/v3/library/albums/album-1", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{album}");
    assert_eq!(album["contribution_id"], json!(id));
    assert_eq!(album["contribution_state"], json!("draft"));
    let (status, ready) = app.call("PUT",
        &format!("/api/v3/library/contributions/{id}/draft"),
        &headers,
        Some(json!({"expected_row_revision": created["row_revision"], "draft": created["draft"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["state"], json!("ready"));

    // A stale revision answers with a code, a sentence and an action.
    let (status, stale) = app
        .call(
            "POST",
            &format!("/api/v3/library/contributions/{id}/musicbrainz/duplicates"),
            &headers,
            Some(json!({"expected_row_revision": created["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], json!("CONTRIBUTION_CHANGED"));
    assert!(stale["error"]["details"]["action"].is_string());

    let (status, checked) = app
        .call(
            "POST",
            &format!("/api/v3/library/contributions/{id}/musicbrainz/duplicates"),
            &headers,
            Some(json!({"expected_row_revision": ready["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["state"], json!("ready"));

    let origin = [
        ("authorization", auth.as_str()),
        ("origin", "https://music.example"),
    ];
    let (status, seed) = app
        .call(
            "POST",
            &format!("/api/v3/library/contributions/{id}/musicbrainz/seed"),
            &origin,
            Some(json!({"expected_row_revision": checked["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{seed}");
    let redirect = seed["fields"]
        .as_array()
        .expect("seed fields")
        .iter()
        .find(|field| field["name"] == json!("redirect_uri"))
        .and_then(|field| field["value"].as_str())
        .expect("redirect uri")
        .to_owned();
    let token = redirect
        .strip_prefix(
            "https://music.example/api/v3/library/contributions/musicbrainz/callback?token=",
        )
        .expect("callback points at this server")
        .to_owned();

    // The editor sends the browser back, here to the v1 path, signed out.
    let back = format!(
        "/api/v1/library/contributions/musicbrainz/callback?token={token}&release_mbid={WRONG}"
    );
    let (status, answer, _) = app.raw("GET", &back, &[], None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        answer["location"],
        format!("/library/contributions/{id}?musicbrainz=returned").as_str()
    );
    let (_, reused, _) = app.raw("GET", &back, &[], None).await;
    assert_eq!(reused["location"], "/library?musicbrainz=callback-error");

    // The returned release does not fit: the matcher refuses it and says
    // why.
    let contribution = format!("/api/v3/library/contributions/{id}");
    let verified = || async {
        app.until(
            "the verification worker settles the contribution",
            || async {
                let (status, body) = app.call("GET", &contribution, &headers, None).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                (body["state"] != json!("verifying")).then_some(())
            },
        )
        .await;
    };
    verified().await;

    let (status, review) = app
        .call(
            "GET",
            &format!("/api/v3/library/contributions/{id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["state"], json!("needs_review"));
    assert_eq!(
        review["review_reason"]["code"],
        json!("ATTACHMENT_WEAK_FIT")
    );
    assert!(review["review_reason"]["action"].is_string());

    // The curator records the right release; it fits and links.
    let (status, verifying) = app
        .call(
            "PUT",
            &format!("/api/v3/library/contributions/{id}/musicbrainz/result"),
            &headers,
            Some(json!({
                "expected_row_revision": review["row_revision"],
                "release_id_or_url": RELEASE,
                "replace_existing_result": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{verifying}");
    assert_eq!(verifying["state"], json!("verifying"));
    verified().await;
    let (status, linked) = app
        .call(
            "GET",
            &format!("/api/v3/library/contributions/{id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["state"], json!("linked"));
    let identity: (String, String, String) = db
        .query_row(
            "SELECT release_mbid, release_group_mbid, decision_source \
             FROM local_album_external_identities WHERE local_album_id = 'album-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("album identity committed");
    assert_eq!(identity, (RELEASE.into(), GROUP.into(), "manual".into()));
    let artist: String = db
        .query_row(
            "SELECT provider_artist_id FROM local_artist_external_identities \
             WHERE local_artist_id = 'artist-1'",
            [],
            |row| row.get(0),
        )
        .expect("artist identity committed");
    assert_eq!(artist, ARTIST_MBID);
    // Every file took its track on the release.
    let tracks: Vec<(String, String, String, i64, i64)> = db
        .prepare(
            "SELECT local_track_id, recording_mbid, release_track_mbid, medium_position, \
             release_track_position FROM local_track_external_identities \
             WHERE decision_source = 'manual' ORDER BY local_track_id",
        )
        .expect("query prepares")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query runs")
        .collect::<Result<_, _>>()
        .expect("rows read");
    assert_eq!(
        tracks,
        vec![
            (
                "t1".into(),
                format!("{RELEASE}-rec-0"),
                format!("{RELEASE}-track-0"),
                1,
                1
            ),
            (
                "t2".into(),
                format!("{RELEASE}-rec-1"),
                format!("{RELEASE}-track-1"),
                1,
                2
            ),
        ]
    );
    rig.stop().await;
}

/// The release the planted files name, built from their own tags so every
/// id agrees.
fn tagged_release(store: &SqliteIdentifyStore, album_id: &str) -> Release {
    tagged_from(&store.album_facts(album_id).unwrap().expect("album facts"))
}

/// [`tagged_release`] for the album identification is asking about.
fn tagged_from(facts: &LocalAlbumFacts) -> Release {
    let first = &facts.tracks[0];
    Release {
        id: first.release_mbid.clone().expect("tagged release"),
        release_group_id: first.release_group_mbid.clone().expect("tagged group"),
        title: facts.title.clone(),
        artists: vec![CreditedArtist {
            id: "a74b1b7f-71a5-4011-9441-d0b5e4122711".to_owned(),
            name: facts.album_artist_name.clone(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: facts
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("release-track-{index}"),
                recording_id: track.recording_mbid.clone().expect("tagged recording"),
                title: track.title.clone(),
                artists: Vec::new(),
                disc: track.disc_number.max(1),
                position: track.track_number,
                absolute_position: index as u32 + 1,
                length_ms: track.duration_secs.map(|seconds| seconds * 1000),
            })
            .collect(),
        ..Release::default()
    }
}

/// One track identity row as `(recording, release, release track, disc,
/// position, decision source)`.
type TrackRow = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
);

fn track_row(db: &Path, track_id: &str) -> TrackRow {
    let conn = droppedneedle_kernel::db::open_connection(db).expect("db opens");
    conn.query_row(
        "SELECT recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source FROM local_track_external_identities \
         WHERE local_track_id = ?1",
        [track_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )
    .expect("track identity row")
}

/// An automatic edition undone to review through the HTTP route: stale
/// revisions are refused, a curator's track row is untouched, and the
/// automatic one returns to its earlier release.
#[tokio::test]
async fn library_journey_undo_automatic_edition() {
    use droppedneedle_library::operations::decisions::live_automatic_edition_undo;

    let rig = Rig::start("lib-operations").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-r/01.flac", "flac_full_01.flac");
    plant(&music, "album-r/02.flac", "flac_full_02.flac");
    // Identification waits until the journey has scripted MusicBrainz for
    // the album the scan finds.
    let paused_at = rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let track_id = {
        rig.scan_store
            .track_at("music", "album-r/01.flac")
            .unwrap()
            .expect("track indexed")
    };
    let (_, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{track_id}"),
            &headers,
            None,
        )
        .await;
    let album = body["album_id"].as_str().expect("album id").to_owned();
    let release = tagged_release(&rig.identify_store, &album);
    let (manual_track, auto_track) = {
        let facts = rig
            .identify_store
            .album_facts(&album)
            .unwrap()
            .expect("facts");
        (
            facts.tracks[0].local_track_id.clone(),
            facts.tracks[1].local_track_id.clone(),
        )
    };
    // Before the automatic pass: a curator's track row, and an automatic
    // row placed on an earlier release.
    let manual_row: TrackRow = (
        "manual-recording".to_owned(),
        None,
        None,
        None,
        None,
        "manual".to_owned(),
    );
    let prior_auto_row: TrackRow = (
        release.tracks[1].recording_id.clone(),
        Some("prior-release".to_owned()),
        Some("prior-release-track".to_owned()),
        Some(1),
        Some(7),
        "automatic".to_owned(),
    );
    {
        let conn = droppedneedle_kernel::db::open_connection(&app.config.library_db_path)
            .expect("db opens");
        for (track, row) in [(&manual_track, &manual_row), (&auto_track, &prior_auto_row)] {
            conn.execute(
                "INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_mbid, release_track_mbid, medium_position, \
                 release_track_position, decision_source, selected_at, row_revision) \
                 VALUES (?1, 'musicbrainz', ?2, ?3, ?4, ?5, ?6, ?7, 1.0, 3)",
                rusqlite::params![track, row.0, row.1, row.2, row.3, row.4, row.5],
            )
            .expect("prior track row");
        }
    }
    let scripted = &rig.identify;
    scripted.set_recall(RecallResult {
        releases: vec![release.clone()],
        ..RecallResult::default()
    });

    // The scan-queued attempt seals the exact edition automatically.
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    let sealed = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("sealed");
    assert_eq!(sealed.release_mbid.as_deref(), Some(release.id.as_str()));
    assert_eq!(
        track_row(&app.config.library_db_path, &manual_track),
        manual_row
    );
    assert_eq!(
        track_row(&app.config.library_db_path, &auto_track)
            .1
            .as_deref(),
        Some(release.id.as_str())
    );

    // Undo needs the revisions the acceptance left; stale ones are refused.
    let (album_revision, identity_revision) = live_automatic_edition_undo(&app.db(), &album)
        .expect("undo reads")
        .expect("undo is live");
    let undo = format!("/api/v3/library/albums/{album}/undo-automatic-edition");
    let stale = json!({
        "expected_album_revision": album_revision,
        "expected_identity_revision": identity_revision + 1,
    });
    let fresh = json!({
        "expected_album_revision": album_revision,
        "expected_identity_revision": identity_revision,
    });
    let (status, body) = app.call("POST", &undo, &headers, Some(stale)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("STALE_REVISION"));
    let (status, body) = app.call("POST", &undo, &headers, Some(fresh.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"], json!("cleared_to_review"));
    assert!(rig.identify_store.album_identity(&album).unwrap().is_none());
    // The curator's track row was never touched; the automatic one is back
    // on its earlier release, disc, and position.
    assert_eq!(
        track_row(&app.config.library_db_path, &manual_track),
        manual_row
    );
    assert_eq!(
        track_row(&app.config.library_db_path, &auto_track),
        prior_auto_row
    );
    let reviews = format!("/api/v3/library/reviews?album_id={album}");
    let (_, body) = app.call("GET", &reviews, &headers, None).await;
    assert_eq!(
        body["reviews"][0]["reason_code"],
        json!("AUTOMATIC_EDITION_CLEARED_TO_REVIEW")
    );
    let review = body["reviews"][0]["id"]
        .as_str()
        .expect("review id")
        .to_owned();
    let (status, _) = app.call("POST", &undo, &headers, Some(fresh)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an undo is used once");

    // Retrying the review identifies the album again, as a scan would,
    // and the job that asked for it finishes.
    let (status, job) = app
        .call(
            "POST",
            &format!("/api/v3/library/reviews/{review}/retry"),
            &headers,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["kind"], json!("explicit_reidentification"));
    rig.identified(&album, 1).await;
    let db = app.db();
    let identity: (Option<String>, String) = db
        .query_row(
            "SELECT release_mbid, decision_source FROM local_album_external_identities \
             WHERE local_album_id = ?1",
            [&album],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("identity sealed again");
    assert_eq!(identity, (Some(release.id.clone()), "automatic".to_owned()));
    let review_state = |id: &str| -> String {
        db.query_row(
            "SELECT state FROM library_identify_reviews WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("review reads")
    };
    assert_eq!(review_state(&review), "dismissed");
    let job_id = job["id"].as_str().expect("job id");
    let (status, ended) = app
        .call(
            "GET",
            &format!("/api/v3/library/operations/{job_id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{ended}");
    assert_eq!(ended["state"], json!("succeeded"), "{ended}");
    assert_eq!(
        ended["terminal_reason"]["code"],
        json!("HANDED_TO_IDENTIFICATION")
    );
    let parked: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_operation_jobs \
             WHERE kind = 'explicit_reidentification' AND state = 'ready'",
            [],
            |row| row.get(0),
        )
        .expect("jobs read");
    assert_eq!(parked, 0, "no retry waits on a choice");

    // An edition someone chose is never retried: the review stays open.
    db.execute(
        "UPDATE local_album_external_identities SET decision_source = 'manual' \
         WHERE local_album_id = ?1",
        [&album],
    )
    .expect("edition chosen");
    db.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         VALUES ('rev-chosen', ?1, 'EDITION_KEPT_OVER_CONFLICT', '[]', 'pending', 5, 5)",
        [&album],
    )
    .expect("review filed");
    let (status, tally) = app
        .call(
            "POST",
            "/api/v3/library/reviews/retry-pending",
            &headers,
            Some(json!({"preview": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tally}");
    assert_eq!(tally["started"], json!(0), "{tally}");
    assert_eq!(
        tally["skipped"].as_array().map(Vec::len),
        Some(1),
        "{tally}"
    );
    assert_eq!(
        tally["skipped"][0]["reason"]["code"],
        json!("EDITION_IS_CHOSEN")
    );
    assert_eq!(tally["skipped"][0]["count"], json!(1));
    let retry_chosen = "/api/v3/library/reviews/rev-chosen/retry";
    let (status, body) = app
        .call("POST", retry_chosen, &headers, Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("EDITION_IS_CHOSEN"));
    assert_eq!(review_state("rev-chosen"), "pending");

    // A folder that keeps its own tags, which DroppedNeedle manages, is
    // never looked up again, whatever was confirmed.
    let page = "/api/v3/settings/library-management";
    let (status, saved) = app.call("GET", page, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let mut settings = saved.clone();
    settings
        .as_object_mut()
        .expect("settings")
        .remove("settings_revision");
    settings["root_assignments"] = json!([{
        "root_id": "music",
        "profile_id": droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID,
        "enabled": true,
    }]);
    let (status, body) = app
        .call(
            "PUT",
            page,
            &headers,
            Some(json!({
                "settings": settings,
                "expected_settings_revision": saved["settings_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    db.execute(
        "UPDATE local_tracks SET applied_policy = 'local_metadata' WHERE local_album_id = ?1",
        [&album],
    )
    .expect("policy set");
    let (status, body) = app
        .call(
            "POST",
            retry_chosen,
            &headers,
            Some(json!({"confirm_local_metadata": true})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("LOCAL_METADATA_MANAGED"));
    assert_eq!(review_state("rev-chosen"), "pending");

    rig.stop().await;
}

/// Two "Low" records with music and no MusicBrainz evidence: listed as a
/// same-name group, opened, refused with a stale revision, then marked
/// distinct, after which the group stays gone.
#[tokio::test]
async fn artist_duplicate_groups_list_open_and_dismiss() {
    let rig = Rig::start("lib-reconcile").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    {
        let conn = droppedneedle_kernel::db::open_connection(&app.config.library_db_path)
            .expect("db opens");
        for n in ["1", "2"] {
            conn.execute_batch(&format!(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                 updated_at) VALUES ('low-{n}', 'Low', 'low', 'group', {n}, {n});
                 INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_id, grouping_source, created_at, updated_at) \
                 VALUES ('album-{n}', 'root', 'g-{n}', 'Album {n}', 'album {n}', 'low-{n}', \
                 'manual', 0, 0);
                 INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
                 VALUES ('album-{n}', 0, 'low-{n}', 'primary');
                 INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
                 path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
                 album_title, album_title_folded, file_format, ingest_source, imported_at, \
                 membership_source) VALUES ('track-{n}', 'album-{n}', 'root', '/m/{n}.flac', \
                 '{n}.flac', 'h{n}', 1, 0, 's', 'Song', 'song', 'Album {n}', 'album {n}', \
                 'flac', 'scan', 0, 'manual');"
            ))
            .expect("seed artist");
        }
    }

    let (status, body) = app
        .call(
            "GET",
            "/api/v3/library/artists/duplicate-groups?search=lo",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    let group = &body["items"][0];
    assert_eq!(group["state"], "same_name_only");
    assert_eq!(group["reason"]["code"], "NAME_MATCH_WITHOUT_PROVIDER_PROOF");
    let group_id = group["id"].as_str().expect("group id").to_owned();
    let path = format!("/api/v3/library/artists/duplicate-groups/{group_id}");

    let (status, detail) = app.call("GET", &path, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["releases"].as_array().map(Vec::len), Some(2));
    let revisions = detail["member_revisions"].clone();

    let dismiss = format!("{path}/dismiss");
    let stale = json!({"expected_member_revisions": {"low-1": 1, "low-2": 9}});
    let (status, body) = app.call("POST", &dismiss, &headers, Some(stale)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let fresh = json!({"expected_member_revisions": revisions});
    let (status, body) = app.call("POST", &dismiss, &headers, Some(fresh)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dismissed_pairs"], 1);

    let (_, body) = app
        .call(
            "GET",
            "/api/v3/library/artists/reconciliation",
            &headers,
            None,
        )
        .await;
    assert_eq!(body["automatically_resolved_count"], 0, "{body}");
    assert_eq!(body["same_name_only_count"], 0, "{body}");
    let (status, _) = app.call("GET", &path, &headers, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    rig.stop().await;
}

/// Removing with files moves them into the root's recycle bin, never
/// deletes them; the catalog rows turn missing and a rescan request
/// covers the album's folder.
#[tokio::test]
async fn removal_recycles_files_and_rescan_targets_the_album() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-remove").await;
    let first = plant(&music, "Band/LP/01.flac", "flac_full_01.flac");
    let second = plant(&music, "Band/LP/02.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .unwrap()
        .expect("track indexed");
    let album: String =
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db opens")
            .query_row(
                "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("album id");
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");

    let rescan = Mutations::new(&library)
        .rescan_album(&album, "admin")
        .expect("rescan requested");
    assert!(!rescan.run_id.is_empty());
    drain_scans(&library).await;

    let removed = Mutations::new(&library)
        .remove_track_now(&track, true, "admin")
        .expect("track removed");
    assert_eq!(removed.track_ids, vec![track.clone()]);
    assert!(!first.exists(), "the file left the album folder");
    let bin = music.join(".recycle");
    assert_eq!(std::fs::read_dir(&bin).expect("bin exists").count(), 1);

    let removed = Mutations::new(&library)
        .remove_album(&album, true, true, "admin")
        .expect("album removed");
    assert_eq!(
        removed.track_ids.len(),
        1,
        "only the indexed track remained"
    );
    assert!(!second.exists());
    assert_eq!(std::fs::read_dir(&bin).expect("bin").count(), 2);
    assert_eq!(
        library
            .scan_store
            .db()
            .query_i64("SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing'")
            .expect("count"),
        2,
        "rows stay, marked missing"
    );
    assert!(
        !Mutations::new(&library)
            .reenable_management(&album, 1, "admin")
            .expect("re-enable answers"),
        "an album that was never excluded reports false"
    );
}

/// A removed album's download and wanted clean-up is written with the
/// removal itself: when the process stops right after the removal's
/// commit, the clean-up still runs once after the restart.
#[tokio::test]
async fn album_cleanup_survives_a_stop_after_the_removal() {
    use droppedneedle_library::mutations::Mutations;

    let rig = Rig::start("lib-remove-cleanup").await;
    let music = rig.app.config.root_app_dir.join("music");
    plant(&music, "Band/LP/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let held = rig.app.config.cache_dir.join("held/01.flac");
    std::fs::create_dir_all(held.parent().expect("held folder")).expect("held folder");
    std::fs::write(&held, b"held").expect("held file");
    rig.scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-gone', 'manual', 0); \
             INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('fan', 'Fan', 'user', '2026-01-01T00:00:00Z'); \
             INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, \
             album_title, status, created_at, updated_at) \
             VALUES ('task-old', 'fan', 'rg-gone', 'Band', 'LP', 'failed', 1767225600000, \
             1767225600000); \
             INSERT INTO acquire_held_entries (id, user_id, origin, group_key, \
             release_group_mbid, state, created_at, expires_at) VALUES (1, 'fan', 'download', \
             'album:rg-gone', 'rg-gone', 'held', 1767225600000, 9999999999999); \
             INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, \
             reason, state, created_at) VALUES (1, 'held', '01.flac', '01.flac', 'soulseek', \
             'weak_match', 'held', 1767225600000); \
             INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, \
             user_id, artist_name, album_title, kind, state, created_at, next_check_at) \
             VALUES ('rg-gone', 'rg-gone', 'fan', 'Band', 'LP', 'missing', 'watching', \
             1767225600000, 1767225600000);"
        ))
        .expect("acquisition rows seeded");

    // The library's own call, as a process stopped right after the
    // removal's commit leaves it: nothing after the removal runs.
    let library = rig.app.state().library.clone();
    let removed = tokio::task::spawn_blocking(move || {
        Mutations::new(&library).remove_album(&album, true, true, "admin")
    })
    .await
    .expect("removal ran")
    .expect("album removed");
    assert_eq!(removed.track_ids, vec![track]);
    let rig = rig.restart().await;

    let db = rig.scan_store.db().clone();
    let held_path = held.clone();
    rig.app
        .until("the removed album's clean-up", || {
            let db = db.clone();
            let held = held_path.clone();
            async move {
                let cleaned = db
                    .query_i64(
                        "SELECT (SELECT status = 'cancelled' FROM download_tasks \
                         WHERE id = 'task-old') \
                         AND (SELECT COUNT(*) FROM acquire_held_files \
                              WHERE state = 'held') = 0 \
                         AND (SELECT state = 'stopped' FROM wanted_watches) \
                         AND (SELECT COUNT(*) FROM durable_jobs \
                              WHERE kind = 'library.album_removed' AND state = 'done') = 1",
                    )
                    .ok()?;
                (cleaned == 1 && !held.exists()).then_some(())
            }
        })
        .await;
    assert_eq!(
        db.query_i64("SELECT COUNT(*) FROM durable_jobs WHERE kind = 'library.album_removed'")
            .expect("count"),
        1,
        "one clean-up for one removal"
    );
    rig.stop().await;
}

/// An upgrade of three tracks where two new files are held, one because
/// another file sits where it would go and one because a file whose name
/// differs only in case sits beside it: the imported file's old copy goes
/// to the recycle bin, and each held file's old copy stays where it is,
/// still listed.
#[tokio::test]
async fn an_upgrade_held_for_one_file_keeps_that_files_old_copy() {
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};

    let (scratch, library, music) = bare_library("lib-upgrade-held").await;
    std::fs::create_dir_all(&music).expect("music folder");
    add_music_root(&library, &music).await;
    let track = |n: u32| ReleaseTrack {
        id: format!("{n}0000000-0000-4000-8000-00000000000b"),
        recording_id: format!("{n}0000000-0000-4000-8000-00000000000c"),
        title: format!("Song {n}"),
        disc: 1,
        position: n,
        absolute_position: n,
        ..ReleaseTrack::default()
    };
    let release = Release {
        id: "10000000-0000-4000-8000-00000000000d".to_owned(),
        release_group_id: "10000000-0000-4000-8000-00000000000e".to_owned(),
        title: "Upgraded".to_owned(),
        artists: vec![CreditedArtist {
            id: "10000000-0000-4000-8000-00000000000a".to_owned(),
            name: "The Band".to_owned(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: vec![track(1), track(2), track(3)],
        ..Release::default()
    };
    let import = |task: &str, fixture: &str, replaces: [Vec<String>; 3]| {
        let files = replaces
            .into_iter()
            .enumerate()
            .map(|(index, replaces)| {
                let source = scratch
                    .to_path_buf()
                    .join(format!("incoming/{task}/{index}-{fixture}"));
                std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
                std::fs::copy(fixtures_dir().join(fixture), &source).expect("download planted");
                ImportSource {
                    path: source,
                    track: index,
                    replaces,
                }
            })
            .collect();
        library
            .imports
            .import_download(&DownloadImport {
                task_id: task.to_owned(),
                origin: ImportOrigin::Download,
                staging: None,
                release: release.clone(),
                files,
                extras: Vec::new(),
                actor: None,
            })
            .expect("imported")
    };
    let relative = |path: &Path| {
        path.strip_prefix(&music)
            .expect("under the root")
            .to_string_lossy()
            .into_owned()
    };
    let first = import(
        "task-mp3",
        "management_full.mp3",
        [Vec::new(), Vec::new(), Vec::new()],
    );
    let old_a = relative(&first.paths[0]);
    let old_b = relative(&first.paths[1]);
    let old_c = relative(&first.paths[2]);
    let (track_a, track_b, track_c) = (
        indexed_at(&library, &old_a),
        indexed_at(&library, &old_b),
        indexed_at(&library, &old_c),
    );
    let old_b_bytes = std::fs::read(music.join(&old_b)).expect("old copy");
    let old_c_bytes = std::fs::read(music.join(&old_c)).expect("old copy");
    // Another file already sits where B's new file would go, and one whose
    // name differs only in case sits beside where C's would go.
    let taken = music.join(old_b.replace(".mp3", ".flac"));
    std::fs::write(&taken, b"someone else's file").expect("planted");
    let c_new = music.join(old_c.replace(".mp3", ".flac"));
    let folded = c_new.with_file_name(
        c_new
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_uppercase(),
    );
    std::fs::write(&folded, b"someone else's file").expect("planted");

    let second = import(
        "task-flac",
        "flac_full_02.flac",
        [
            vec![track_a.clone()],
            vec![track_b.clone()],
            vec![track_c.clone()],
        ],
    );
    let held: Vec<usize> = second.skipped.iter().map(|(index, _)| *index).collect();
    assert_eq!(held, [1, 2], "{:?}", second.skipped);
    assert!(second.blocked.is_empty(), "{:?}", second.blocked);
    let db = scratch_db(&scratch);
    let availability = |id: &str| -> String {
        db.query_row(
            "SELECT availability FROM local_tracks WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("track row")
    };
    assert!(!music.join(&old_a).exists(), "A's old copy left");
    assert_eq!(availability(&track_a), "missing");
    assert_eq!(
        std::fs::read(music.join(&old_b)).expect("B's old copy"),
        old_b_bytes,
        "the held file's old copy stays"
    );
    assert_eq!(availability(&track_b), "indexed");
    assert_eq!(
        std::fs::read(music.join(&old_c)).expect("C's old copy"),
        old_c_bytes,
        "the held file's old copy stays"
    );
    assert_eq!(availability(&track_c), "indexed");
    assert!(!c_new.exists(), "C's new file is not placed");
    assert_eq!(
        std::fs::read(&taken).expect("the file in the way"),
        b"someone else's file"
    );
    let binned: Vec<PathBuf> = walk_files(&music.join(".recycle"));
    assert_eq!(
        binned.len(),
        1,
        "only A's old copy is in the bin: {binned:?}"
    );
}

/// "Put back" never replaces a file that now sits where the removed one
/// was: it is refused, the newer file is untouched and the removed file
/// stays in the recycle bin, where a later "Put back" still finds it.
#[tokio::test]
async fn put_back_refuses_an_occupied_path() {
    use droppedneedle_library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-put-back").await;
    let original = plant(&music, "Band/LP/01.flac", "flac_full_01.flac");
    let removed_bytes = std::fs::read(&original).expect("removed file reads");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .unwrap()
        .expect("track indexed");
    let album: String =
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db opens")
            .query_row(
                "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("album id");
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");

    let action = Mutations::new(&library)
        .recycle_album_files(&album, std::slice::from_ref(&track), "admin")
        .expect("file recycled")
        .removal_id
        .expect("removal id");
    let recycled = walk_files(&music.join(".recycle"));
    assert_eq!(recycled.len(), 1, "one file in the bin");
    plant(&music, "Band/LP/01.flac", "flac_full_02.flac");
    let newer_bytes = std::fs::read(&original).expect("newer file reads");
    assert_ne!(newer_bytes, removed_bytes);

    let refused = Mutations::new(&library).put_back(&album, &action, "admin");
    assert!(
        matches!(
            &refused,
            Err(MutationError::Conflict(reason)) if reason.code == "RESTORE_PATH_OCCUPIED"
        ),
        "{refused:?}"
    );
    drain_scans(&library).await;
    assert_eq!(
        std::fs::read(&original).expect("newer file reads"),
        newer_bytes,
        "the newer file is untouched"
    );
    assert_eq!(
        std::fs::read(&recycled[0]).expect("recycled copy reads"),
        removed_bytes,
        "the removed file is still in the bin"
    );

    std::fs::remove_file(&original).expect("newer file moved away");
    let missing = Mutations::new(&library)
        .put_back(&album, &action, "admin")
        .expect("put back once the path is free");
    assert!(missing.is_empty(), "{missing:?}");
    assert_eq!(
        std::fs::read(&original).expect("restored file reads"),
        removed_bytes
    );
    assert_eq!(
        library
            .scan_store
            .track_at("music", "Band/LP/01.flac")
            .unwrap()
            .as_deref(),
        Some(track.as_str()),
        "the track is listed again at once, under its own id"
    );
}

/// A removal stopped at any step of its write ends, once recovery has
/// run, with each file in the recycle bin and its track missing, or at
/// home with its track listed: never one without the other. So does
/// Remove extras, whose undo record exists exactly when its tracks left.
/// A removal whose tracks change while it waits for recovery is put back.
#[tokio::test]
async fn a_stopped_removal_keeps_each_file_with_its_track() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::CrashPoint;

    let (scratch, library, music) = bare_library("lib-remove-stopped").await;
    // (where it stops, Remove extras, the rows change before recovery)
    let mut cases: Vec<(CrashPoint, bool, bool)> = CrashPoint::all()
        .into_iter()
        .flat_map(|point| [(point, false, false), (point, true, false)])
        .collect();
    cases.push((CrashPoint::BeforeCatalogCommit, false, true));
    // Untagged files group by folder: one album per case.
    for index in 0..cases.len() {
        plant(&music, &format!("Case{index}/01.flac"), "flac_no_tags.flac");
        plant(&music, &format!("Case{index}/02.flac"), "flac_no_tags.flac");
    }
    add_music_root(&library, &music).await;
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    let conn = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let availability = |track: &str| -> String {
        conn.query_row(
            "SELECT availability FROM local_tracks WHERE id = ?1",
            [track],
            |row| row.get(0),
        )
        .expect("track row")
    };
    let bin = music.join(".recycle");

    for (index, (point, extras, rows_change)) in cases.into_iter().enumerate() {
        let case = format!("case {index} ({point:?}, extras {extras})");
        let paths: Vec<PathBuf> = ["01.flac", "02.flac"]
            .iter()
            .map(|name| music.join(format!("Case{index}/{name}")))
            .collect();
        let tracks: Vec<String> = ["01.flac", "02.flac"]
            .iter()
            .map(|name| {
                library
                    .scan_store
                    .track_at("music", &format!("Case{index}/{name}"))
                    .unwrap()
                    .expect("track indexed")
            })
            .collect();
        let album: String = conn
            .query_row(
                "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                [&tracks[0]],
                |row| row.get(0),
            )
            .expect("album id");
        let on_album: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1",
                [&album],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(on_album, 2, "{case}: one album per folder");
        let binned_before = walk_files(&bin).len();

        library.publishing.arm_publish_crash(Some(point));
        let mutations = Mutations::new(&library);
        let answer = if extras {
            mutations
                .recycle_album_files(&album, &tracks, "admin")
                .map(|_| ())
        } else {
            mutations
                .remove_album(&album, true, true, "admin")
                .map(|_| ())
        };
        library.publishing.arm_publish_crash(None);
        let committed = matches!(
            point,
            CrashPoint::AfterCatalogCommit | CrashPoint::DuringCleanup
        );
        assert_eq!(
            answer.is_ok(),
            committed,
            "{case}: only a removal that committed answers that it removed: {answer:?}"
        );
        if rows_change {
            conn.execute(
                "UPDATE local_tracks SET stat_revision = 'changed' WHERE id = ?1",
                [&tracks[0]],
            )
            .expect("row changes");
        }
        library.publishing.publish_tick().expect("recovery runs");

        let mut gone = 0;
        for (path, track) in paths.iter().zip(&tracks) {
            let home = path.exists();
            let listed = availability(track) == "indexed";
            assert_eq!(home, listed, "{case}: {} and its track", path.display());
            if !home {
                gone += 1;
            }
        }
        assert_eq!(
            walk_files(&bin).len() - binned_before,
            gone,
            "{case}: every file that left is in the bin"
        );
        assert!(
            walk_files(&music.join(format!("Case{index}")))
                .iter()
                .all(|path| path.file_name().is_some_and(|name| {
                    !name
                        .to_string_lossy()
                        .starts_with(".droppedneedle-management-")
                })),
            "{case}: no hidden copy is left"
        );
        let expected_gone = if rows_change { 0 } else { 2 };
        assert_eq!(gone, expected_gone, "{case}");
        let listed = Mutations::new(&library)
            .album_removals(&album)
            .expect("removals read");
        assert_eq!(
            listed.len(),
            usize::from(gone == 2),
            "{case}: the removal is offered for Put back exactly when it took the files"
        );
    }
}

/// One album of two untagged files per folder `Case{index}`, every file
/// taken out by Remove extras. Answers the album, its tracks, their
/// paths and the undo's action id.
fn removed_extras(
    library: &LibrarySetup,
    music: &Path,
    conn: &rusqlite::Connection,
    folder: &str,
) -> (String, Vec<String>, Vec<PathBuf>, String) {
    use droppedneedle_library::mutations::Mutations;

    let names = ["01.flac", "02.flac"];
    let paths: Vec<PathBuf> = names
        .iter()
        .map(|name| music.join(format!("{folder}/{name}")))
        .collect();
    let tracks: Vec<String> = names
        .iter()
        .map(|name| {
            library
                .scan_store
                .track_at("music", &format!("{folder}/{name}"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album: String = conn
        .query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [&tracks[0]],
            |row| row.get(0),
        )
        .expect("album id");
    let action = Mutations::new(library)
        .recycle_album_files(&album, &tracks, "admin")
        .expect("files recycled")
        .removal_id
        .expect("removal id");
    (album, tracks, paths, action)
}

fn add_admin(library: &LibrarySetup) {
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
}

fn availability_of(conn: &rusqlite::Connection, track: &str) -> String {
    conn.query_row(
        "SELECT availability FROM local_tracks WHERE id = ?1",
        [track],
        |row| row.get(0),
    )
    .expect("track row")
}

fn restored_actions(conn: &rusqlite::Connection, album: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM library_catalog_actions \
         WHERE action_kind = 'files_put_back' AND local_album_id = ?1",
        [album],
        |row| row.get(0),
    )
    .expect("count")
}

fn no_hidden_files(dir: &Path) -> bool {
    walk_files(dir).iter().all(|path| {
        path.file_name().is_some_and(|name| {
            !name
                .to_string_lossy()
                .starts_with(".droppedneedle-management-")
        })
    })
}

/// A removed album's page offers its removal while the files are in the
/// recycle bin. Putting it back lists every track again under its own id
/// on the same album, with its plays and favourites, and the removal is
/// offered no more. A removal whose copies the bin's expiry deleted is
/// not offered, and putting it back changes nothing.
#[tokio::test]
async fn a_removed_album_is_put_back_from_its_page() {
    use droppedneedle_library::mutations::{MutationError, Mutations, RemovalKind};

    let (scratch, library, music) = bare_library("lib-put-back-album").await;
    plant(&music, "Back/01.flac", "flac_no_tags.flac");
    plant(&music, "Back/02.flac", "flac_no_tags.flac");
    plant(&music, "Gone/01.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let track_at = |rel: &str| -> String {
        library
            .scan_store
            .track_at("music", rel)
            .unwrap()
            .expect("track indexed")
    };
    let album_of = |track: &str| -> String {
        conn.query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [track],
            |row| row.get(0),
        )
        .expect("album id")
    };
    let rels = ["Back/01.flac", "Back/02.flac"];
    let tracks: Vec<String> = rels.iter().map(|rel| track_at(rel)).collect();
    let album = album_of(&tracks[0]);
    conn.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('admin', 'track', '{track}', 0), ('admin', 'album', '{album}', 0); \
         INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
         artist_name, played_at) VALUES ('play-1', 'admin', '{track}', 'Song', 'Artist', \
         '2026-01-01T00:00:00Z');",
        track = tracks[0]
    ))
    .expect("favourites and a play");
    let mutations = Mutations::new(&library);

    let removal = mutations
        .remove_album(&album, true, true, "admin")
        .expect("album removed")
        .removal_id
        .expect("removal id");
    let listed = mutations.album_removals(&album).expect("removals read");
    assert_eq!(listed.len(), 1, "the removal is offered: {listed:?}");
    assert_eq!(
        (
            listed[0].id.as_str(),
            listed[0].kind,
            listed[0].files,
            listed[0].held
        ),
        (removal.as_str(), RemovalKind::Album, 2, false)
    );

    let missing = mutations
        .put_back(&album, &removal, "admin")
        .expect("album put back");
    assert!(missing.is_empty(), "{missing:?}");
    for (track, rel) in tracks.iter().zip(rels) {
        assert!(music.join(rel).is_file(), "{rel} is home");
        assert_eq!(&track_at(rel), track, "{rel} keeps its track id");
        assert_eq!(availability_of(&conn, track), "indexed");
        assert_eq!(album_of(track), album, "{rel} is on its album again");
    }
    let kept: i64 = conn
        .query_row(
            &format!(
                "SELECT (SELECT COUNT(*) FROM library_user_favorites \
                 WHERE (item_kind = 'track' AND item_id = '{track}') \
                 OR (item_kind = 'album' AND item_id = '{album}')) \
                 + (SELECT COUNT(*) FROM library_play_history \
                 WHERE id = 'play-1' AND local_track_id = '{track}') \
                 + (SELECT has_indexed_track FROM local_albums WHERE id = '{album}')",
                track = tracks[0]
            ),
            [],
            |row| row.get(0),
        )
        .expect("counts");
    assert_eq!(
        kept, 4,
        "favourites, the play and the album's files are back"
    );
    assert!(
        mutations
            .album_removals(&album)
            .expect("removals read")
            .is_empty(),
        "a removal put back is offered no more"
    );

    let gone_track = track_at("Gone/01.flac");
    let gone_album = album_of(&gone_track);
    let gone = mutations
        .remove_album(&gone_album, true, true, "admin")
        .expect("album removed")
        .removal_id
        .expect("removal id");
    assert_eq!(
        mutations.album_removals(&gone_album).expect("read").len(),
        1
    );
    conn.execute(
        "UPDATE library_publish_replacements SET recycled_at = recycled_at - 40 * 86400 \
         WHERE replaced_track_id = ?1",
        [&gone_track],
    )
    .expect("copy ages past the retention");
    library
        .publishing
        .recycle_expiry_tick(&|| true)
        .expect("expiry runs");
    assert!(
        mutations
            .album_removals(&gone_album)
            .expect("removals read")
            .is_empty(),
        "an expired copy is not offered"
    );
    let refused = mutations.put_back(&gone_album, &gone, "admin");
    assert!(
        matches!(
            &refused,
            Err(MutationError::Conflict(reason)) if reason.code == "RECYCLED_FILES_GONE"
        ),
        "{refused:?}"
    );
    assert_eq!(availability_of(&conn, &gone_track), "missing");
}

/// A removed track's copy in the bin, and its row.
fn copy_of(conn: &rusqlite::Connection, track: &str) -> (PathBuf, String) {
    conn.query_row(
        "SELECT recycle_to, journal_id FROM library_publish_replacements \
         WHERE replaced_track_id = ?1",
        [track],
        |row| Ok((PathBuf::from(row.get::<_, String>(0)?), row.get(1)?)),
    )
    .expect("a copy in the bin")
}

/// Whether row `journal_id`'s copy is marked as having left the bin.
fn left_bin(conn: &rusqlite::Connection, journal_id: &str) -> bool {
    conn.query_row(
        "SELECT expired_at IS NOT NULL FROM library_publish_replacements WHERE journal_id = ?1",
        [journal_id],
        |row| row.get(0),
    )
    .expect("row")
}

/// Emptying the bin deletes, however young, every copy that reached it
/// before the moment its count was read and still holds the removed
/// bytes, and marks its row. A copy changed in the bin is kept and named,
/// one a put-back is using stays, and a removal made after that moment is
/// untouched.
#[tokio::test]
async fn emptying_the_bin_deletes_unchanged_copies_and_keeps_the_rest() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::expiry::KeptWhy;

    let (scratch, library, music) = bare_library("lib-empty-bin").await;
    for folder in ["A", "B", "C", "D"] {
        plant(&music, &format!("{folder}/01.flac"), "flac_no_tags.flac");
    }
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let mutations = Mutations::new(&library);
    let remove = |folder: &str| -> String {
        let track = library
            .scan_store
            .track_at("music", &format!("{folder}/01.flac"))
            .unwrap()
            .expect("track indexed");
        let album: String = conn
            .query_row(
                "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("album");
        mutations
            .remove_album(&album, true, true, "admin")
            .expect("album removed");
        track
    };
    let [a, b, c] = ["A", "B", "C"].map(&remove);
    let (a_copy, a_row) = copy_of(&conn, &a);
    let (b_copy, b_row) = copy_of(&conn, &b);
    let (c_copy, c_row) = copy_of(&conn, &c);
    let a_size = std::fs::metadata(&a_copy).expect("copy").len();
    std::fs::write(&b_copy, b"changed in the bin").expect("copy changes");
    conn.execute(
        "INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
         staged, staged_sha256, track_id, state) VALUES ('pb-000', 'pb', 'put_back', \
         'music', 'C/01.flac', '.dn-pb', 'x', ?1, 'staged')",
        [&c],
    )
    .expect("a put-back holds C");
    conn.execute(
        "UPDATE library_publish_replacements SET recycled_at = recycled_at - 10",
        [],
    )
    .expect("the copies reached the bin a little earlier");
    let before = library.clock.now_ms();
    let d = remove("D");
    let (d_copy, d_row) = copy_of(&conn, &d);

    let report = library
        .publishing
        .empty_recycle_bin(before, &|| true)
        .expect("the bin empties");

    assert!(report.finished);
    assert!(!a_copy.exists(), "an unchanged copy goes");
    assert!(left_bin(&conn, &a_row));
    assert_eq!((report.deleted, report.deleted_bytes), (1, a_size));
    assert_eq!(std::fs::read(&b_copy).expect("kept"), b"changed in the bin");
    assert!(!left_bin(&conn, &b_row));
    assert_eq!(
        report
            .kept_copies
            .iter()
            .map(|kept| (kept.path.clone(), kept.why))
            .collect::<Vec<_>>(),
        [(b_copy, KeptWhy::Changed)]
    );
    assert!(c_copy.is_file(), "a copy a put-back uses stays");
    assert!(!left_bin(&conn, &c_row));
    assert_eq!(report.waited, 1);
    assert!(d_copy.is_file(), "a removal made after the count stays");
    assert!(!left_bin(&conn, &d_row));
}

/// The bin page lists a removal with its files and their size, offers
/// Put back, and lists it no more once its files are back.
#[tokio::test]
async fn the_bin_lists_a_removal_until_it_is_put_back() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::bin_contents::{self, BinKind};

    let (scratch, library, music) = bare_library("lib-bin-page").await;
    plant(&music, "Back/01.flac", "flac_no_tags.flac");
    plant(&music, "Back/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let size: u64 = ["Back/01.flac", "Back/02.flac"]
        .iter()
        .map(|rel| std::fs::metadata(music.join(rel)).expect("file").len())
        .sum();
    let track = library
        .scan_store
        .track_at("music", "Back/01.flac")
        .unwrap()
        .expect("track indexed");
    let album: String = conn
        .query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [&track],
            |row| row.get(0),
        )
        .expect("album");
    let mutations = Mutations::new(&library);
    let removal = mutations
        .remove_album(&album, true, true, "admin")
        .expect("album removed")
        .removal_id
        .expect("removal id");
    let sandbox = library.publishing.expiry_sandbox();

    let page = bin_contents::page(&library.db, sandbox.as_ref(), None, 50).expect("bin read");
    assert_eq!(page.items.len(), 1, "{page:?}");
    let listed = &page.items[0];
    assert_eq!(
        (
            listed.id.as_str(),
            listed.kind,
            listed.files,
            listed.bytes,
            listed.can_put_back
        ),
        (removal.as_str(), BinKind::Album, 2, size, true)
    );
    assert_eq!(
        listed.album.as_ref().map(|album| album.id.as_str()),
        Some(album.as_str())
    );

    mutations
        .put_back(&album, &removal, "admin")
        .expect("album put back");
    let page = bin_contents::page(&library.db, sandbox.as_ref(), None, 50).expect("bin read");
    assert!(
        page.items.is_empty(),
        "a removal put back has left the bin: {page:?}"
    );
}

/// A put-back stopped at any step ends, once recovery has run, with
/// every file either back home with its track listed and the undo spent,
/// or still in the recycle bin with its track missing and the undo there
/// to use: never a file at home without its track, never one in neither
/// place. Only a put-back that committed answers that it put files back.
/// It holds both when the bin is on the album's disk, where each file
/// comes home as a hard link of its copy in the bin, and when it is not,
/// where each file is copied.
#[tokio::test]
async fn a_put_back_stopped_at_any_step_ends_in_the_bin_or_home() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::CrashPoint;
    use droppedneedle_library::publish::place::Seams;
    use std::os::unix::fs::MetadataExt as _;

    let (scratch, library, music) = bare_library("lib-put-back-stopped").await;
    let mut points = CrashPoint::all();
    points.push(CrashPoint::AfterFirstFileStaged);
    points.push(CrashPoint::AfterTempWritten);
    let cases: Vec<(CrashPoint, bool)> = points
        .into_iter()
        .flat_map(|point| [(point, false), (point, true)])
        .collect();
    for index in 0..cases.len() {
        plant(&music, &format!("Case{index}/01.flac"), "flac_no_tags.flac");
        plant(&music, &format!("Case{index}/02.flac"), "flac_no_tags.flac");
    }
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let bin = music.join(".recycle");

    for (index, (point, copied)) in cases.into_iter().enumerate() {
        let case = format!("case {index} ({point:?}, copied {copied})");
        let (album, tracks, paths, action) =
            removed_extras(&library, &music, &conn, &format!("Case{index}"));
        let binned = walk_files(&bin).len();

        library.publishing.set_publish_seams(Seams {
            copy_put_back: copied,
            ..Seams::default()
        });
        library.publishing.arm_publish_crash(Some(point));
        let answer = Mutations::new(&library).put_back(&album, &action, "admin");
        library.publishing.arm_publish_crash(None);
        library.publishing.set_publish_seams(Seams::default());
        if matches!(
            point,
            CrashPoint::AfterStage
                | CrashPoint::AfterFirstFileStaged
                | CrashPoint::AfterTempWritten
        ) {
            let temps: Vec<PathBuf> = walk_files(&music.join(format!("Case{index}")))
                .into_iter()
                .filter(|path| {
                    path.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .starts_with(".droppedneedle-management-")
                    })
                })
                .collect();
            assert!(!temps.is_empty(), "{case}: a temp waits beside home");
            for temp in &temps {
                let links = std::fs::metadata(temp).expect("temp stats").nlink();
                assert_eq!(
                    links,
                    if copied { 1 } else { 2 },
                    "{case}: {} is a link of its copy in the bin only on one disk",
                    temp.display()
                );
            }
        }
        let committed = matches!(
            point,
            CrashPoint::AfterCatalogCommit | CrashPoint::DuringCleanup
        );
        assert_eq!(
            answer.is_ok(),
            committed,
            "{case}: only a put-back that committed answers that it put files back: {answer:?}"
        );
        library.publishing.publish_tick().expect("recovery runs");

        let home: Vec<bool> = paths.iter().map(|path| path.exists()).collect();
        assert!(
            home.iter().all(|at| *at == home[0]),
            "{case}: the files end together: {home:?}"
        );
        for (path, track) in paths.iter().zip(&tracks) {
            let listed = availability_of(&conn, track) == "indexed";
            assert_eq!(
                path.exists(),
                listed,
                "{case}: {} and its track",
                path.display()
            );
        }
        let back = home[0];
        let unwinds = matches!(
            point,
            CrashPoint::AfterFirstFileStaged | CrashPoint::AfterTempWritten
        );
        assert_eq!(
            back, !unwinds,
            "{case}: staged in full resumes, else unwinds"
        );
        assert_eq!(
            walk_files(&bin).len(),
            if back { binned - 2 } else { binned },
            "{case}: a copy leaves the bin exactly when its file is home"
        );
        assert_eq!(
            restored_actions(&conn, &album),
            i64::from(back),
            "{case}: the undo"
        );
        assert!(
            no_hidden_files(&music.join(format!("Case{index}"))),
            "{case}: no hidden copy is left"
        );
    }
}

/// Recycling tracks whose files are all gone already moves no file, but
/// still takes the tracks out of the library.
#[tokio::test]
async fn recycling_tracks_whose_files_are_gone_takes_them_out() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-recycle-all-gone").await;
    plant(&music, "Gone/01.flac", "flac_no_tags.flac");
    plant(&music, "Gone/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let tracks: Vec<String> = ["01.flac", "02.flac"]
        .iter()
        .map(|name| {
            library
                .scan_store
                .track_at("music", &format!("Gone/{name}"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album: String = conn
        .query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [&tracks[0]],
            |row| row.get(0),
        )
        .expect("album id");
    for name in ["01.flac", "02.flac"] {
        std::fs::remove_file(music.join(format!("Gone/{name}"))).expect("file goes");
    }

    Mutations::new(&library)
        .recycle_album_files(&album, &tracks, "admin")
        .expect("the tracks are taken out");
    for track in &tracks {
        assert_eq!(availability_of(&conn, track), "missing");
    }
}

/// Putting back files that were all moved home by hand already moves no
/// file, but lists their tracks again and spends the undo.
#[tokio::test]
async fn putting_back_files_already_home_lists_them_again() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-put-back-all-home").await;
    plant(&music, "Home/01.flac", "flac_no_tags.flac");
    plant(&music, "Home/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Home");
    for path in &paths {
        let name = path.file_name().expect("name");
        let copy = walk_files(&music.join(".recycle"))
            .into_iter()
            .find(|copy| copy.file_name() == Some(name))
            .expect("its copy");
        std::fs::rename(&copy, path).expect("moved home by hand");
    }

    Mutations::new(&library)
        .put_back(&album, &action, "admin")
        .expect("the files are listed again");
    for track in &tracks {
        assert_eq!(availability_of(&conn, track), "indexed");
    }
    assert_eq!(restored_actions(&conn, &album), 1, "the undo is spent");
}

/// A file that turns up where a removed file was, after the put-back
/// checked the place and before the file lands, is never replaced: the
/// placing call refuses it, nothing is put back, and the removed copy
/// stays in the recycle bin.
#[tokio::test]
async fn a_file_that_lands_just_before_a_put_back_survives() {
    use droppedneedle_library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-put-back-late").await;
    plant(&music, "Late/01.flac", "flac_no_tags.flac");
    plant(&music, "Late/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Late");
    let binned = walk_files(&music.join(".recycle"));
    assert_eq!(binned.len(), 2);

    library.publishing.set_publish_hooks(
        Some(Box::new(|dest| {
            std::fs::write(dest, b"someone else's file").expect("planted");
        })),
        None,
    );
    let answer = Mutations::new(&library).put_back(&album, &action, "admin");
    library.publishing.set_publish_hooks(None, None);
    assert!(
        matches!(
            &answer,
            Err(MutationError::Conflict(reason)) if reason.code == "RESTORE_PATH_OCCUPIED"
        ),
        "{answer:?}"
    );
    assert_eq!(
        std::fs::read(&paths[0]).expect("the file in the way"),
        b"someone else's file"
    );
    assert!(!paths[1].exists(), "nothing more was placed");
    assert_eq!(
        walk_files(&music.join(".recycle")),
        binned,
        "the copies stay"
    );
    for track in &tracks {
        assert_eq!(availability_of(&conn, track), "missing");
    }
    assert_eq!(restored_actions(&conn, &album), 0, "the undo is not spent");
    assert!(no_hidden_files(&music.join("Late")));
}

/// A removed file whose copy is gone from the recycle bin stays out; the
/// rest come back, each with its own permission bits and modified time
/// and its track listed at once, and the answer and the audit name the missing one.
/// When no copy is left, nothing is put back and the undo stays.
#[tokio::test]
async fn put_back_names_a_file_gone_from_the_bin() {
    use droppedneedle_library::mutations::{MutationError, Mutations};
    use std::os::unix::fs::PermissionsExt as _;

    let (scratch, library, music) = bare_library("lib-put-back-gone").await;
    for name in ["01.flac", "02.flac"] {
        let path = plant(&music, &format!("Gone/{name}"), "flac_no_tags.flac");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("mode set");
    }
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let modified = std::fs::metadata(music.join("Gone/01.flac"))
        .and_then(|meta| meta.modified())
        .expect("modified time");
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Gone");
    let copy_of_02 = walk_files(&music.join(".recycle"))
        .into_iter()
        .find(|path| path.ends_with("02.flac"))
        .expect("02's copy");
    std::fs::remove_file(&copy_of_02).expect("copy goes");

    let missing = Mutations::new(&library)
        .put_back(&album, &action, "admin")
        .expect("the rest come back");
    assert_eq!(missing, ["Gone/02.flac"]);
    assert!(paths[0].exists() && !paths[1].exists());
    assert_eq!(
        std::fs::metadata(&paths[0])
            .expect("home")
            .permissions()
            .mode()
            & 0o777,
        0o640,
        "the file keeps its permission bits"
    );
    assert_eq!(
        std::fs::metadata(&paths[0])
            .and_then(|meta| meta.modified())
            .expect("modified time"),
        modified,
        "and its modified time"
    );
    assert_eq!(
        library
            .scan_store
            .track_at("music", "Gone/01.flac")
            .unwrap()
            .as_deref(),
        Some(tracks[0].as_str()),
        "listed at once, under its own id"
    );
    assert_eq!(availability_of(&conn, &tracks[1]), "missing");
    let after: String = conn
        .query_row(
            "SELECT after_json FROM library_catalog_actions \
             WHERE action_kind = 'files_put_back' AND local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("audit row");
    assert!(after.contains("Gone/02.flac"), "{after}");

    let again = Mutations::new(&library)
        .recycle_album_files(&album, std::slice::from_ref(&tracks[0]), "admin")
        .expect("removed again")
        .removal_id
        .expect("removal id");
    for copy in walk_files(&music.join(".recycle")) {
        std::fs::remove_file(copy).expect("copy goes");
    }
    for _ in 0..2 {
        let refused = Mutations::new(&library).put_back(&album, &again, "admin");
        assert!(
            matches!(
                &refused,
                Err(MutationError::Conflict(reason)) if reason.code == "RECYCLED_FILES_GONE"
            ),
            "{refused:?}"
        );
    }
}

/// A copy changed in the recycle bin in a way that keeps its size and
/// modified time is not put back, and is named; the rest come back.
#[tokio::test]
async fn put_back_names_a_copy_changed_in_the_bin() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-put-back-edited").await;
    plant(&music, "Edited/01.flac", "flac_no_tags.flac");
    plant(&music, "Edited/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Edited");
    let copy = walk_files(&music.join(".recycle"))
        .into_iter()
        .find(|path| path.ends_with("02.flac"))
        .expect("02's copy");
    let modified = std::fs::metadata(&copy)
        .and_then(|meta| meta.modified())
        .expect("modified time");
    let mut bytes = std::fs::read(&copy).expect("copy reads");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&copy)
        .expect("copy opens");
    std::os::unix::fs::FileExt::write_all_at(&file, &bytes[last..], last as u64)
        .expect("copy edited in place");
    file.set_modified(modified).expect("modified time kept");
    drop(file);

    let missing = Mutations::new(&library)
        .put_back(&album, &action, "admin")
        .expect("the rest come back");
    assert_eq!(missing, ["Edited/02.flac"]);
    assert!(paths[0].exists() && !paths[1].exists());
    assert_eq!(availability_of(&conn, &tracks[0]), "indexed");
    assert_eq!(availability_of(&conn, &tracks[1]), "missing");
    assert!(copy.exists(), "the changed copy stays in the bin");
}

/// A removed file still waiting under its hidden name for a recycle bin
/// that cannot take it yet is put back from there, and the undo is spent
/// only by a put-back that happened. Once the bin works again, nothing
/// reaches it, and the removal's own row settles.
#[tokio::test]
async fn put_back_takes_a_file_still_waiting_for_the_bin() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-put-back-waiting").await;
    plant(&music, "Wait/01.flac", "flac_no_tags.flac");
    plant(&music, "Wait/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    // Something takes the bin folder's name once the removal's checks
    // passed: no file can move in.
    let blocker = music.join(".recycle");
    let planted = blocker.clone();
    library.publishing.set_publish_hooks(
        None,
        Some(Box::new(move |_| {
            if !planted.exists() {
                std::fs::write(&planted, b"in the way").expect("bin blocked");
            }
        })),
    );
    let conn = scratch_db(&scratch);
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Wait");
    library.publishing.set_publish_hooks(None, None);
    assert!(paths.iter().all(|path| !path.exists()));
    assert!(
        !no_hidden_files(&music.join("Wait")),
        "the files wait hidden"
    );

    let answer = Mutations::new(&library).put_back(&album, &action, "admin");
    assert!(answer.is_ok(), "{answer:?}");
    for (path, track) in paths.iter().zip(&tracks) {
        assert!(path.exists(), "{} is home", path.display());
        assert_eq!(availability_of(&conn, track), "indexed");
    }
    assert_eq!(restored_actions(&conn, &album), 1);
    assert!(
        no_hidden_files(&music.join("Wait")),
        "no hidden copy is left"
    );

    std::fs::remove_file(&blocker).expect("bin unblocked");
    library.publishing.publish_tick().expect("recovery runs");
    assert!(
        walk_files(&music.join(".recycle")).is_empty(),
        "nothing reached the bin"
    );
    let unsettled: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM library_publish_journal \
             WHERE kind = 'remove' AND state <> 'cleaned'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(unsettled, 0, "the removal's rows settled");
    assert!(paths.iter().all(|path| path.exists()));
}

/// A removed track whose row gave its path up to a hidden name (another
/// file stood there for a while) gets its path back with its file: same
/// id, listed at its old place.
#[tokio::test]
async fn put_back_returns_a_track_that_gave_its_path_up() {
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-put-back-gone-row").await;
    plant(&music, "Moved/01.flac", "flac_no_tags.flac");
    plant(&music, "Moved/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let (album, tracks, paths, action) = removed_extras(&library, &music, &conn, "Moved");
    // As a scan leaves it after other music stood at the path.
    let hidden = format!(".droppedneedle-management-gone/{}/Moved/01.flac", tracks[0]);
    conn.execute(
        "UPDATE local_tracks SET relative_path = ?2, path_hash = ?2 WHERE id = ?1",
        rusqlite::params![tracks[0], hidden],
    )
    .expect("row gives its path up");

    let answer = Mutations::new(&library).put_back(&album, &action, "admin");
    assert!(answer.is_ok(), "{answer:?}");
    assert!(paths.iter().all(|path| path.exists()));
    for (name, track) in ["Moved/01.flac", "Moved/02.flac"].iter().zip(&tracks) {
        assert_eq!(
            library
                .scan_store
                .track_at("music", name)
                .unwrap()
                .as_deref(),
            Some(track.as_str()),
            "{name} is listed under its own id"
        );
    }
}

/// A link made inside the library folder after a removal is never
/// followed home: the put-back is refused, nothing is written where the
/// link points, the copy stays in the bin, and the undo still works once
/// the link is gone.
#[tokio::test]
async fn put_back_never_follows_a_link_in_the_library() {
    use droppedneedle_library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-put-back-link").await;
    plant(&music, "Linked/LP/01.flac", "flac_no_tags.flac");
    plant(&music, "Linked/LP/02.flac", "flac_no_tags.flac");
    add_music_root(&library, &music).await;
    add_admin(&library);
    let conn = scratch_db(&scratch);
    let (album, _, paths, action) = removed_extras(&library, &music, &conn, "Linked/LP");
    let outside = scratch.to_path_buf().join("outside");
    std::fs::create_dir_all(&outside).expect("outside folder");
    let folder = music.join("Linked/LP");
    std::fs::remove_dir_all(&folder).expect("folder goes");
    std::os::unix::fs::symlink(&outside, &folder).expect("link made");

    let refused = Mutations::new(&library).put_back(&album, &action, "admin");
    assert!(
        matches!(
            &refused,
            Err(MutationError::Conflict(reason)) if reason.code == "FILE_OUTSIDE_ROOT"
        ),
        "{refused:?}"
    );
    assert!(
        walk_files(&outside).is_empty(),
        "nothing went through the link"
    );
    assert_eq!(
        walk_files(&music.join(".recycle")).len(),
        2,
        "the copies stay"
    );

    std::fs::remove_file(&folder).expect("link goes");
    let answer = Mutations::new(&library).put_back(&album, &action, "admin");
    assert!(answer.is_ok(), "{answer:?}");
    assert!(paths.iter().all(|path| path.exists()));
}

/// A library folder configured through a symlinked parent takes managed
/// writes like any other: a tag edit writes the file, and a removal moves
/// it into the folder's recycle bin and lists the track as missing. A
/// folder reached through a link into another library folder takes none.
#[tokio::test]
async fn a_library_folder_reached_through_a_symlink_takes_writes() {
    use droppedneedle_library::management::models::TagEditMode;
    use droppedneedle_library::management::service::Management;
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::management::tag_edit::TagEditRequest;
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, _music) = bare_library("lib-linked-folder").await;
    let real = scratch.to_path_buf().join("real");
    let file = plant(&real, "music/Band/LP/01.flac", "management_full.flac");
    let linked = scratch.to_path_buf().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("link made");
    add_music_root(&library, &linked.join("music")).await;
    let track = library
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    crate::common::library::operations(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    let mut settings = lm::initial_settings("");
    for profile in &mut settings.profiles {
        for field in &mut profile.metadata.fields {
            if droppedneedle_library::management::enrich::CREDIT_FIELDS
                .iter()
                .any(|(name, _)| *name == field.field)
            {
                field.mode = droppedneedle_kernel::runtime_config::sections::FieldMode::Disabled;
            }
        }
    }
    library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");
    lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
        .get_settings()
        .expect("settings normalized");

    let edit = Management::new(&library)
        .create_tag_edit(
            TagEditRequest {
                local_track_id: track.clone(),
                mode: TagEditMode::SaveOverride,
                expected_settings_revision: lm::settings_revision(
                    &library
                        .settings
                        .store()
                        .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
                        .expect("settings read"),
                ),
                expected_policy_revision: library.core.policy_revision().expect("policy revision"),
                fields: vec![("title".to_owned(), serde_json::json!("Linked Title"))],
                idempotency_key: Some("linked-edit".to_owned()),
            },
            "admin",
        )
        .expect("edit preview");
    assert!(management_step(&library).await, "edit planning runs");
    let applied = management_apply(&library, &edit).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    assert_eq!(
        read_fields(&file).expect("tags read")[&TagField::Title],
        vec!["Linked Title".to_owned()]
    );

    let bytes = std::fs::read(&file).expect("file read");
    Mutations::new(&library)
        .remove_track_now(&track, true, "admin")
        .expect("removal");
    assert!(!file.exists(), "the file left the folder");
    let binned: Vec<PathBuf> = std::fs::read_dir(real.join("music/.recycle"))
        .expect("the folder's bin")
        .map(|entry| entry.expect("entry").path().join("01.flac"))
        .collect();
    assert_eq!(binned.len(), 1, "{binned:?}");
    assert_eq!(std::fs::read(&binned[0]).expect("bin copy"), bytes);
    let availability: String =
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db opens")
            .query_row(
                "SELECT availability FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("track row");
    assert_eq!(availability, "missing");

    // A second folder reached through a link into the first takes no
    // managed writes: two ids would manage one set of files.
    let second = plant(&real, "music/Band/LP2/02.flac", "flac_full_02.flac");
    let other = scratch.to_path_buf().join("other");
    std::os::unix::fs::symlink(real.join("music"), &other).expect("link made");
    let inner = other.join("Band");
    library
        .scans
        .add_root(
            Some("inner".to_owned()),
            inner.to_string_lossy().into_owned(),
            droppedneedle_library::scan::EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(&library).await;
    let through_link = library
        .scan_store
        .track_at("inner", "LP2/02.flac")
        .unwrap()
        .expect("track indexed");
    let refused = Mutations::new(&library).remove_track_now(&through_link, true, "admin");
    assert!(
        matches!(
            &refused,
            Err(droppedneedle_library::mutations::MutationError::Conflict(reason))
                if reason.code == "FILE_OUTSIDE_ROOT"
        ),
        "{refused:?}"
    );
    assert!(second.exists(), "the file stays");
}

/// One edition operation: a person chooses another edition (partial
/// mapping), automatic passes leave it alone, undo takes it back, and
/// "Let DroppedNeedle choose" returns the album to its best fit.
#[tokio::test]
async fn library_journey_choose_edition() {
    const OTHER: &str = "0f6e7a1c-2b3d-4e5f-8a9b-1c2d3e4f5a6b";
    let rig = Rig::start("lib-edition").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-e/01.flac", "flac_full_01.flac");
    plant(&music, "album-e/02.flac", "flac_full_02.flac");
    // Identification waits until the journey has scripted MusicBrainz for
    // the album the scan finds.
    let paused_at = rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let track_id = {
        rig.scan_store
            .track_at("music", "album-e/01.flac")
            .unwrap()
            .expect("track indexed")
    };
    let (_, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{track_id}"),
            &headers,
            None,
        )
        .await;
    let album = body["album_id"].as_str().expect("album id").to_owned();
    let fit = tagged_release(&rig.identify_store, &album);
    // Another edition of the group holding only the first song.
    let mut other = fit.clone();
    other.id = OTHER.to_owned();
    other.tracks.truncate(1);
    other.tracks[0].id = "other-track-0".to_owned();
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone(), other.clone()],
        ..RecallResult::default()
    });
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let (_, body) = app.call("GET", &edition, &headers, None).await;
    assert_eq!(body["state"], json!("confirmed"), "{body}");
    assert_eq!(body["release_mbid"], json!(fit.id));

    // The plan for the current edition: both files keep their place.
    let (status, body) = app
        .call("GET", &format!("{edition}/plan"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_current"], json!(true));
    assert_eq!(
        (
            &body["kept_count"],
            &body["missing_count"],
            &body["extra_count"]
        ),
        (&json!(2), &json!(0), &json!(0)),
        "{body}"
    );
    // The plan for the smaller edition, before choosing it: one file
    // keeps its place, the other becomes an extra, nothing changes yet.
    let (status, body) = app
        .call(
            "GET",
            &format!("{edition}/plan?release_mbid={OTHER}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_current"], json!(false));
    assert_eq!(
        (
            &body["kept_count"],
            &body["missing_count"],
            &body["extra_count"]
        ),
        (&json!(1), &json!(0), &json!(1)),
        "{body}"
    );
    let unchanged = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("row");
    assert_eq!(unchanged.release_mbid.as_deref(), Some(fit.id.as_str()));

    // Choose the smaller edition: one file placed, one kept as an extra.
    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": OTHER})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["retag_files"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["extra_track_ids"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["status"]["state"], json!("chosen"));
    assert_eq!(body["status"]["undo_available"], json!(true));

    // An automatic pass never flips the choice.
    let (status, _) = app
        .call(
            "POST",
            "/api/v3/library/identify",
            &headers,
            Some(json!({"album_id": album})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    rig.identified(&album, 1).await;
    let kept = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("row");
    assert_eq!(kept.release_mbid.as_deref(), Some(OTHER));

    // Undo brings back the automatic best fit.
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("confirmed"));
    assert_eq!(body["release_mbid"], json!(fit.id));

    // Choose again, then let DroppedNeedle choose: best fit again.
    let (status, _) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": OTHER})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = app.call("DELETE", &edition, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.identified(&album, 1).await;
    let back = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("row");
    assert_eq!(back.release_mbid.as_deref(), Some(fit.id.as_str()));

    // A release MusicBrainz does not know is refused with a reason.
    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": "11111111-2222-4333-8444-555555555555"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("EDITION_NOT_FOUND"));
    rig.stop().await;
}

/// The album's "leave unmanaged" as the album page reads it.
async fn management_hold(app: &TestApp, headers: &[(&str, &str)], album: &str) -> Value {
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album}"),
            headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["management_hold"].clone()
}

/// Leave the album unmanaged by a person's own hand, as the album page
/// would have it.
fn exclude_by_hand(db: &rusqlite::Connection, album: &str) {
    db.execute(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) \
         SELECT ?1, 'manual', id, 5 FROM auth_users WHERE role = 'admin' LIMIT 1",
        [album],
    )
    .expect("album left unmanaged");
}

fn catalog_actions(db: &rusqlite::Connection, album: &str, kind: &str) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = ?2",
        [album, kind],
        |row| row.get(0),
    )
    .expect("actions read")
}

fn active_manifest(db: &rusqlite::Connection, album: &str) -> Option<String> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT manifest_id FROM library_custom_edition_active WHERE local_album_id = ?1",
        [album],
        |row| row.get(0),
    )
    .optional()
    .expect("active manifest reads")
}

/// A custom edition sealed directly on the album: refused when the
/// release's title contradicts the files or the album moved since it was
/// read; sealed with one audit row; undone back to the release, the
/// manifest kept. A person's "leave unmanaged" survives both the seal and
/// a later "Use this edition", and undoing that choice brings the custom
/// edition back on its own manifest.
#[tokio::test]
async fn custom_edition_is_sealed_on_the_album_and_undone() {
    const OTHER: &str = "0f6e7a1c-2b3d-4e5f-8a9b-1c2d3e4f5a6b";
    let rig = Rig::start("lib-custom-direct").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, _) = identified_album(&rig, &music, "album-c").await;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let custom = format!("{edition}/custom");
    let db = app.db();
    let revision = edition_status(app, &headers, &edition).await["identity_revision"].clone();

    // A release whose title contradicts the files is refused.
    let mut renamed = fit.clone();
    renamed.id = OTHER.to_owned();
    renamed.title = "Nothing Like These Files".to_owned();
    rig.identify.set_recall(RecallResult {
        releases: vec![renamed],
        ..RecallResult::default()
    });
    let (status, body) = app
        .call(
            "POST",
            &custom,
            &headers,
            Some(json!({"release_mbid": OTHER, "expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("CUSTOM_EDITION_NOT_SEALABLE"));
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone()],
        ..RecallResult::default()
    });
    // So is an identity revision the album has moved past.
    let (status, body) = app
        .call(
            "POST",
            &custom,
            &headers,
            Some(json!({"release_mbid": fit.id, "expected_identity_revision": 999})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("STALE_REVISION"));
    let body = edition_status(app, &headers, &edition).await;
    assert_eq!(body["identity_revision"], revision);
    assert_eq!(body["edition_kind"], json!("release"));
    assert_eq!(catalog_actions(&db, &album, "custom_edition"), 0);

    let (status, body) = app
        .call(
            "POST",
            &custom,
            &headers,
            Some(json!({"release_mbid": fit.id, "expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["edition_kind"], json!("custom"), "{body}");
    assert_eq!(body["state"], json!("chosen"));
    assert_eq!(body["release_mbid"], Value::Null);
    assert_eq!(body["undo_available"], json!(true));
    assert_eq!(catalog_actions(&db, &album, "custom_edition"), 1);
    assert!(active_manifest(&db, &album).is_some());

    // Undo: the release is back, the custom edition no longer active, its
    // manifest kept.
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["release_mbid"], json!(fit.id), "{body}");
    assert_eq!(body["edition_kind"], json!("release"));
    assert_eq!(active_manifest(&db, &album), None);
    let manifests: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_custom_edition_manifests WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("manifests read");
    assert_eq!(manifests, 1);

    // With a person's "leave unmanaged": sealing and then choosing a
    // release both keep it.
    exclude_by_hand(&db, &album);
    let revision = edition_status(app, &headers, &edition).await["identity_revision"].clone();
    let (status, body) = app
        .call(
            "POST",
            &custom,
            &headers,
            Some(json!({"release_mbid": fit.id, "expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sealed = active_manifest(&db, &album).expect("custom edition active");
    assert_eq!(
        management_hold(app, &headers, &album).await["kind"],
        json!("your_choice")
    );
    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": fit.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(active_manifest(&db, &album), None);
    assert_eq!(
        management_hold(app, &headers, &album).await["kind"],
        json!("your_choice")
    );

    // Undoing that choice brings the custom edition back as it was.
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["edition_kind"], json!("custom"), "{body}");
    assert_eq!(active_manifest(&db, &album), Some(sealed));
    assert_eq!(
        management_hold(app, &headers, &album).await["kind"],
        json!("your_choice")
    );
    rig.stop().await;
}

/// "Leave unmanaged" made directly on an automatically matched album: a
/// stale revision changes nothing; otherwise the release goes, the group
/// stays as the person's choice, the album is held as their choice and a
/// queued placement is dropped. Undo brings the release and placements
/// back and lifts the hold, or keeps a hold the person had set before.
/// "Manage it again" lifts it too.
#[tokio::test]
async fn leave_unmanaged_on_the_album_and_undo() {
    let rig = Rig::start("lib-leave-direct").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, _) = identified_album(&rig, &music, "album-l").await;
    let track = rig
        .scan_store
        .track_at("music", "album-l/01.flac")
        .unwrap()
        .expect("track indexed");
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let leave = format!("{edition}/leave-unmanaged");
    let db = app.db();
    let identity = || {
        db.query_row(
            "SELECT decision_source, release_group_mbid, release_mbid, row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("identity reads")
    };
    let matched = identity();
    assert_eq!(matched.0, "automatic");
    let revision = matched.3;

    let (status, body) = app
        .call(
            "POST",
            &leave,
            &headers,
            Some(json!({"expected_identity_revision": revision + 1})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("STALE_REVISION"));
    assert_eq!(identity(), matched);
    assert_eq!(management_hold(app, &headers, &album).await, Value::Null);
    assert_eq!(catalog_actions(&db, &album, "leave_unmanaged"), 0);

    db.execute(
        "INSERT INTO library_edition_remap_queue \
         (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
         VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
        rusqlite::params![album, fit.id],
    )
    .expect("remap row queued");
    // The match waits in Unconfirmed matches; the person's word settles it.
    let match_state = || {
        use rusqlite::OptionalExtension as _;
        db.query_row(
            "SELECT state FROM library_album_match_state WHERE local_album_id = ?1",
            [&album],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("match state reads")
    };
    db.execute(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, candidates_json, updated_at) \
         VALUES (?1, 'unconfirmed', 'CLOSE_CALL', ?2, '[]', 1)",
        rusqlite::params![album, fit.id],
    )
    .expect("match left unconfirmed");
    let (status, body) = app
        .call(
            "POST",
            &leave,
            &headers,
            Some(json!({"expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["release_mbid"], Value::Null, "{body}");
    assert_eq!(body["edition_kind"], json!("group_only"));
    assert_eq!(body["state"], json!("chosen"));
    assert_eq!(body["placing"], Value::Null);
    let left = identity();
    assert_eq!(
        (left.0.as_str(), left.1.as_str(), left.2.as_deref()),
        ("manual", matched.1.as_str(), None)
    );
    assert_eq!(placement(&db, &track).0, None);
    assert_eq!(match_state(), None);
    assert_eq!(
        management_hold(app, &headers, &album).await["kind"],
        json!("your_choice")
    );
    assert_eq!(catalog_actions(&db, &album, "leave_unmanaged"), 1);

    // Undo: the release and its placements are back, the hold is lifted.
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["release_mbid"], json!(fit.id), "{body}");
    assert_eq!(placement(&db, &track).0.as_deref(), Some(fit.id.as_str()));
    assert_eq!(match_state().as_deref(), Some("unconfirmed"));
    assert_eq!(management_hold(app, &headers, &album).await, Value::Null);

    // A hold the person set before stays through the undo, and "Manage it
    // again" lifts it.
    exclude_by_hand(&db, &album);
    let (status, body) = app
        .call(
            "POST",
            &leave,
            &headers,
            Some(json!({"expected_identity_revision": identity().3})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call("POST", &format!("{edition}/undo"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hold = management_hold(app, &headers, &album).await;
    assert_eq!(hold["kind"], json!("your_choice"), "{hold}");
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album}/management/re-enable"),
            &headers,
            Some(json!({"expected_exclusion_revision": hold["exclusion_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reenabled"], json!(true));
    assert_eq!(management_hold(app, &headers, &album).await, Value::Null);
    rig.stop().await;
}

/// A two-file album in `music`, identified as the release its tags name.
/// Returns the album, that release and the identification switch's
/// revision.
async fn identified_album(rig: &Rig, music: &Path, folder: &str) -> (String, Release, u64) {
    plant(music, &format!("{folder}/01.flac"), "flac_full_01.flac");
    plant(music, &format!("{folder}/02.flac"), "flac_full_02.flac");
    let paused_at = rig.pause_identification().await;
    rig.app.add_library_folder("music", music).await;
    let track_id = rig
        .scan_store
        .track_at("music", &format!("{folder}/01.flac"))
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track_id)
        .unwrap()
        .expect("album");
    let fit = tagged_release(&rig.identify_store, &album);
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone()],
        ..RecallResult::default()
    });
    let control = rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    (album, fit, control)
}

async fn edition_status(app: &TestApp, headers: &[(&str, &str)], edition: &str) -> Value {
    let (status, body) = app.call("GET", edition, headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The edition status says what happens next: while the chosen edition's
/// files are placed (and how often MusicBrainz failed), while the album is
/// identified again after "Let DroppedNeedle choose", the kind of edition
/// and its revision, and that nothing writes the files by itself in a
/// folder managed by hand.
#[tokio::test]
async fn edition_status_says_what_happens_next() {
    let rig = Rig::start("lib-edition-status").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, control) = identified_album(&rig, &music, "album-s").await;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let status = || edition_status(app, &headers, &edition);

    let body = status().await;
    assert_eq!(body["release_mbid"], json!(fit.id), "{body}");
    assert_eq!(body["edition_kind"], json!("release"));
    assert!(body["identity_revision"].as_i64() > Some(0), "{body}");
    assert_eq!(body["placing"], Value::Null);
    assert_eq!(body["identifying"], json!(false));
    assert_eq!(body["files_by_itself"], json!(false), "a manual folder");
    assert_eq!(body["writing"], json!(false));

    // A remap row seeded with a far-off `not_before`, so the loop never
    // takes it: placing, then waiting on MusicBrainz, then done.
    let db = app.db();
    db.execute(
        "INSERT INTO library_edition_remap_queue \
         (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
         VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
        rusqlite::params![album, fit.id],
    )
    .expect("remap row queued");
    assert_eq!(status().await["placing"], json!({"attempts": 0}));
    db.execute(
        "UPDATE library_edition_remap_queue SET attempts = 1 WHERE local_album_id = ?1",
        [&album],
    )
    .expect("row updated");
    assert_eq!(status().await["placing"], json!({"attempts": 1}));
    db.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        [&album],
    )
    .expect("row removed");
    assert_eq!(status().await["placing"], Value::Null);

    // "Let DroppedNeedle choose" while identification is paused: the job
    // stays open and the status says so until it has run.
    let (code, body) = app
        .call(
            "POST",
            "/api/v3/library/identification/pause",
            &headers,
            Some(json!({"expected_revision": control})),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let paused_at = body["row_revision"].as_u64().expect("revision");
    let (code, body) = app.call("DELETE", &edition, &headers, None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["identifying"], json!(true), "{body}");
    assert_eq!(status().await["identifying"], json!(true));
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    assert_eq!(status().await["identifying"], json!(false));
    rig.stop().await;
}

/// In a folder managed automatically the app writes a chosen edition's
/// files by itself, unless the album is left unmanaged; right after a
/// choice that write is still to happen.
#[tokio::test]
async fn edition_status_says_when_the_app_writes_the_files() {
    let rig = Rig::start("lib-edition-writes").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, _) = identified_album(&rig, &music, "album-a").await;
    activate_automatic_folder(app, &headers, false).await;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let (status, body) = app.call("GET", &edition, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["files_by_itself"], json!(true), "{body}");

    let db = app.db();
    db.execute(
        "INSERT INTO library_management_exclusions \
         (local_album_id, reason, excluded_by_user_id, excluded_at) \
         SELECT ?1, 'user', id, 0 FROM auth_users WHERE role = 'admin' LIMIT 1",
        [&album],
    )
    .expect("album left unmanaged");
    let (_, body) = app.call("GET", &edition, &headers, None).await;
    assert_eq!(body["files_by_itself"], json!(false), "{body}");
    assert_eq!(body["writing"], json!(false));
    db.execute(
        "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
        [&album],
    )
    .expect("exclusion removed");

    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": fit.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"]["files_by_itself"], json!(true), "{body}");
    assert_eq!(body["status"]["writing"], json!(true), "{body}");
    let status = || async {
        let (code, body) = app.call("GET", &edition, &headers, None).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        body
    };
    let writes = |body: &Value| (body["files_by_itself"].clone(), body["writing"].clone());

    // Parked, or waiting on a preview a person must look at: nothing is
    // about to be written.
    db.execute(
        "UPDATE library_management_automatic_queue SET not_before = 1e15, \
         last_reason_code = 'MANAGEMENT_FAILED' WHERE local_album_id = ?1",
        [&album],
    )
    .expect("row parked");
    assert_eq!(writes(&status().await), (json!(true), json!(false)));
    db.execute(
        "UPDATE library_management_automatic_queue SET not_before = 1e12, \
         last_reason_code = 'PREVIEW_BLOCKED' WHERE local_album_id = ?1",
        [&album],
    )
    .expect("row waits on a blocked preview");
    assert_eq!(writes(&status().await), (json!(true), json!(false)));

    // The app stepped back from the album: it writes nothing until the
    // album is handed back, and a hand-back on its way counts as done.
    db.execute(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, 'files_changed', 0)",
        [&album],
    )
    .expect("album taken over");
    assert_eq!(writes(&status().await), (json!(false), json!(false)));
    db.execute(
        "UPDATE library_album_takeovers SET handed_back_at = 1, hand_back_retry_at = 1000000000000000 \
         WHERE local_album_id = ?1",
        [&album],
    )
    .expect("hand-back stamped");
    assert_eq!(status().await["files_by_itself"], json!(true));
    db.execute(
        "DELETE FROM library_album_takeovers WHERE local_album_id = ?1",
        [&album],
    )
    .expect("mark lifted");

    // A custom edition, where the folder does not manage custom editions
    // by itself.
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET release_mbid = NULL, \
         row_revision = row_revision + 1 WHERE local_album_id = '{album}'; \
         INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
         release_group_mbid, album_title, album_artist_name, source_album_revision, \
         input_revision, content_hash, sealed_by_user_id, sealed_at) \
         SELECT 'manifest-1', '{album}', 1, '{group}', 'Sealed', 'Sealed', 1, 'input', 'hash', \
         id, 0 FROM auth_users WHERE role = 'admin' LIMIT 1; \
         INSERT INTO library_custom_edition_tracks (manifest_id, ordinal, local_track_id, \
         source_track_revision, stat_revision, tag_revision, title, artist_name, album_title, \
         album_artist_name, disc_number, track_number) SELECT 'manifest-1', rowid, id, 1, 's', \
         't', 'Sealed', 'Sealed', 'Sealed', 'Sealed', 1, rowid FROM local_tracks \
         WHERE local_album_id = '{album}' AND availability = 'indexed'; \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, activated_at) \
         VALUES ('{album}', 'manifest-1', 0);",
        group = fit.release_group_id,
    ))
    .expect("custom edition sealed");
    let body = status().await;
    assert_eq!(body["edition_kind"], json!("custom"), "{body}");
    assert_eq!(writes(&body), (json!(false), json!(false)));
    // Where the folder manages custom editions by itself, it writes this one.
    activate_automatic_folder(app, &headers, true).await;
    assert_eq!(status().await["files_by_itself"], json!(true));
    // A manifest that lists only some of the album's files is no custom
    // edition to the worker: it waits for a release, and so does the status.
    db.execute_batch(&format!(
        "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
         release_group_mbid, album_title, album_artist_name, source_album_revision, \
         input_revision, content_hash, sealed_by_user_id, sealed_at) \
         SELECT 'manifest-2', '{album}', 2, '{group}', 'Sealed', 'Sealed', 1, 'input', 'hash-2', \
         id, 0 FROM auth_users WHERE role = 'admin' LIMIT 1; \
         INSERT INTO library_custom_edition_tracks (manifest_id, ordinal, local_track_id, \
         source_track_revision, stat_revision, tag_revision, title, artist_name, album_title, \
         album_artist_name, disc_number, track_number) SELECT 'manifest-2', 0, id, 1, 's', \
         't', 'Sealed', 'Sealed', 'Sealed', 'Sealed', 1, 1 FROM local_tracks \
         WHERE local_album_id = '{album}' AND availability = 'indexed' LIMIT 1; \
         UPDATE library_custom_edition_active SET manifest_id = 'manifest-2' \
         WHERE local_album_id = '{album}';",
        group = fit.release_group_id,
    ))
    .expect("manifest gone stale");
    let body = status().await;
    assert_eq!(body["edition_kind"], json!("custom"), "{body}");
    assert_eq!(writes(&body), (json!(false), json!(false)));
    db.execute(
        "DELETE FROM library_custom_edition_active WHERE local_album_id = ?1",
        [&album],
    )
    .expect("custom edition let go");
    // Only a release group: nothing is written until a release is chosen.
    let body = status().await;
    assert_eq!(body["edition_kind"], json!("group_only"), "{body}");
    assert_eq!(writes(&body), (json!(false), json!(false)));

    // An unconfirmed match waits for a person before anything is written.
    db.execute(
        "UPDATE local_album_external_identities SET release_mbid = ?2, \
         row_revision = row_revision + 1 WHERE local_album_id = ?1",
        [&album, &fit.id],
    )
    .expect("release back");
    assert_eq!(status().await["files_by_itself"], json!(true));
    db.execute(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, updated_at) \
         VALUES (?1, 'unconfirmed', 'LOOSE_MATCH', 0)",
        [&album],
    )
    .expect("match unconfirmed");
    assert_eq!(writes(&status().await), (json!(false), json!(false)));
    db.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        [&album],
    )
    .expect("match confirmed");

    // An edition with no track for one of the files: that file can never
    // be written from it, so once the files are placed nothing is written
    // by itself.
    let mut other = fit.clone();
    other.id = "3e4f5a6b-7c8d-4e9f-8a0b-1c2d3e4f5a6b".to_owned();
    other.tracks[1].id = "other-track-1".to_owned();
    other.tracks[1].recording_id = "c3d4e5f6-0000-4000-8000-000000000002".to_owned();
    other.tracks[1].length_ms = other.tracks[1].length_ms.map(|length| length + 45_000);
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone(), other.clone()],
        ..RecallResult::default()
    });
    let (code, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": other.id})),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let body = app
        .until("the files are placed", || async {
            let body = status().await;
            (body["placing"].is_null() && body["identifying"] == json!(false)).then_some(body)
        })
        .await;
    assert_eq!(body["release_mbid"], json!(other.id), "{body}");
    assert_eq!(writes(&body), (json!(false), json!(false)), "{body}");
    rig.stop().await;
}

/// A management preview and a dry run take the policy revision the
/// library settings page shows, right after a folder is saved there. Once
/// a rule changes, the old revision is refused.
#[tokio::test]
async fn management_previews_take_the_settings_policy_revision() {
    let started = TestApp::start("lib-mgmt-revision").await;
    let app = &started;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    std::fs::create_dir_all(&music).expect("music dir");

    let library = "/api/v3/settings/library";
    let save_library = |roots: Value| async move {
        let (status, mut settings) = app.call("GET", library, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{settings}");
        let revision = settings["policy_revision"].clone();
        settings["library_roots"] = roots;
        let (status, body) = app
            .call(
                "PUT",
                library,
                &headers,
                Some(json!({"settings": settings, "expected_policy_revision": revision})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["policy_revision"]
            .as_str()
            .expect("policy revision")
            .to_owned()
    };
    let root = |rules: Value| {
        json!([{
            "id": "music",
            "label": "Music",
            "path": music.to_string_lossy(),
            "policy": "automatic",
            "rules": rules,
        }])
    };
    let saved = save_library(root(json!([]))).await;
    let (status, shown) = app.call("GET", library, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    let policy = shown["policy_revision"].clone();
    assert_eq!(policy, json!(saved));

    let (status, mut management) = app
        .call("GET", "/api/v3/settings/library-management", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{management}");
    let settings_revision = management["settings_revision"].clone();
    let preview = |policy: Value| {
        let body = json!({
            "selection": {"kind": "roots", "ids": ["music"]},
            "profile_id": droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID,
            "expected_settings_revision": settings_revision.clone(),
            "expected_policy_revision": policy,
        });
        async move {
            app.call(
                "POST",
                "/api/v3/library/management/previews",
                &headers,
                Some(body),
            )
            .await
        }
    };
    let (status, body) = preview(policy.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    if let Some(fields) = management.as_object_mut() {
        fields.remove("settings_revision");
    }
    management["root_assignments"] = json!([{
        "root_id": "music",
        "profile_id": droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID,
        "enabled": true,
        "automatic_scan_discovered": true,
    }]);
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/settings/library-management/activation-previews",
            &headers,
            Some(json!({
                "root_id": "music",
                "settings": management,
                "expected_settings_revision": settings_revision.clone(),
                "expected_policy_revision": policy.clone(),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    save_library(root(json!([
        {"id": "live", "relative_path": "Live", "policy": "excluded"}
    ])))
    .await;
    let (status, body) = preview(policy).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("library folders"), "{body}");
    started.stop().await;
}

/// Turn automatic management on for the `music` folder the way the
/// settings page does: a dry run, then its confirmation. `custom_editions`
/// lets it manage custom editions by itself too.
async fn activate_automatic_folder(app: &TestApp, headers: &[(&str, &str)], custom_editions: bool) {
    let (status, mut settings) = app
        .call("GET", "/api/v3/settings/library-management", headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    let revision = settings["settings_revision"]
        .as_str()
        .expect("settings revision")
        .to_owned();
    if let Some(fields) = settings.as_object_mut() {
        fields.remove("settings_revision");
    }
    let assignments = settings["root_assignments"]
        .as_array_mut()
        .expect("root assignments");
    match assignments
        .iter_mut()
        .find(|assignment| assignment["root_id"] == json!("music"))
    {
        Some(assignment) => {
            assignment["enabled"] = json!(true);
            assignment["automatic_scan_discovered"] = json!(true);
            assignment["automatic_custom_editions"] = json!(custom_editions);
        }
        None => assignments.push(json!({
            "root_id": "music",
            "profile_id": droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID,
            "enabled": true,
            "automatic_scan_discovered": true,
            "automatic_custom_editions": custom_editions,
        })),
    }
    let (status, roots) = app
        .call("GET", "/api/v3/library/roots", headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{roots}");
    let (status, dry_run) = app
        .call(
            "POST",
            "/api/v3/settings/library-management/activation-previews",
            headers,
            Some(json!({
                "root_id": "music",
                "settings": settings,
                "expected_settings_revision": revision,
                "expected_policy_revision": roots["policy_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{dry_run}");
    let job = dry_run["job_id"].as_str().expect("dry run job").to_owned();
    let detail = format!("/api/v3/settings/library-management/activation-previews/{job}");
    let detail = detail.as_str();
    app.until("the dry run is ready", || async move {
        let (status, body) = app.call("GET", detail, headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (body["ready_for_confirmation"] == json!(true)).then_some(())
    })
    .await;
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/settings/library-management/activation-confirmations",
            headers,
            Some(json!({
                "settings": settings,
                "proofs": [{
                    "root_id": "music",
                    "job_id": job,
                    "preview_token": dry_run["preview_token"],
                }],
                "expected_settings_revision": revision,
                "confirmation": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// An album nobody matched, in a folder that keeps its own tags, says the
/// folder is why: identification never runs there by itself. One with a
/// file under a folder that identifies does not.
#[tokio::test]
async fn an_unmatched_album_in_a_keep_tags_folder_says_why() {
    let rig = Rig::start("lib-edition-keep-tags").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-k/01.flac", "flac_full_01.flac");
    plant(&music, "album-k/02.flac", "flac_full_02.flac");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/roots",
            &headers,
            Some(json!({
                "id": "music",
                "path": music.to_string_lossy(),
                "policy": "local_metadata",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let album = app
        .until("the album is indexed", || {
            let album = rig
                .scan_store
                .track_at("music", "album-k/01.flac")
                .unwrap()
                .and_then(|track| rig.scan_store.album_for_track(&track).unwrap());
            async move { album }
        })
        .await;
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album}/edition"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("unidentified"), "{body}");
    assert_eq!(body["reason"]["code"], json!("FOLDER_KEEPS_TAGS"), "{body}");
    assert_eq!(body["edition_kind"], json!("none"));
    assert_eq!(body["identity_revision"], json!(0));

    let second = app
        .until("the second file is indexed", || {
            let track = rig.scan_store.track_at("music", "album-k/02.flac").unwrap();
            async move { track }
        })
        .await;
    assert_eq!(
        rig.scan_store.album_for_track(&second).unwrap().as_ref(),
        Some(&album)
    );
    app.db()
        .execute(
            "UPDATE local_tracks SET applied_policy = 'automatic' WHERE id = ?1",
            [&second],
        )
        .expect("one file identifies");
    let (_, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album}/edition"),
            &headers,
            None,
        )
        .await;
    assert_eq!(body["reason"]["code"], json!("UNIDENTIFIED"), "{body}");
    rig.stop().await;
}

/// The plan of another edition names a file that is another version of one
/// of its tracks (same title and artist, 45 s longer, another recording)
/// instead of listing it as an extra beside a missing track, still counts
/// that track missing, and carries the edition's facts and the files'
/// lengths. Any signed-in user reads the album's own plan; planning
/// another release and searching for one are for curators.
#[tokio::test]
async fn edition_plan_names_another_version_of_a_track() {
    const OTHER: &str = "0f6e7a1c-2b3d-4e5f-8a9b-1c2d3e4f5a6b";
    const STRANGER: &str = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";
    let rig = Rig::start("lib-edition-plan").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    let (album, fit, _) = identified_album(&rig, &music, "album-p").await;
    let mut other = fit.clone();
    other.id = OTHER.to_owned();
    other.date = Some("2004-05-01".to_owned());
    other.country = Some("GB".to_owned());
    other.tracks[1].id = "other-track-1".to_owned();
    other.tracks[1].recording_id = "a1b2c3d4-0000-4000-8000-000000000002".to_owned();
    other.tracks[1].length_ms = other.tracks[1].length_ms.map(|length| length + 45_000);
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone(), other.clone()],
        ..RecallResult::default()
    });
    let plan = format!("/api/v3/library/albums/{album}/edition/plan");
    let (status, body) = app
        .call(
            "GET",
            &format!("{plan}?release_mbid={OTHER}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            &body["kept_count"],
            &body["missing_count"],
            &body["other_version_count"],
            &body["extra_count"],
        ),
        (&json!(1), &json!(1), &json!(1), &json!(0)),
        "{body}"
    );
    let second = body["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["fate"] != json!("keep"))
        .expect("the unplaced file");
    assert_eq!(second["fate"], json!("other_version"), "{body}");
    assert_eq!(
        (&second["edition_disc_number"], &second["edition_position"]),
        (
            &json!(other.tracks[1].disc),
            &json!(other.tracks[1].position)
        )
    );
    assert!(second["length_ms"].as_u64() > Some(0), "{body}");
    assert_eq!(body["edition"]["date"], json!("2004-05-01"));
    assert_eq!(body["edition"]["country"], json!("GB"));
    assert_eq!(body["same_album"], json!(true));

    // The same title by another artist, on another album: not another
    // version, just a file the edition has no track for.
    let mut stranger = other.clone();
    stranger.id = STRANGER.to_owned();
    stranger.release_group_id = "5d6e7f80-1a2b-4c3d-8e9f-0a1b2c3d4e5f".to_owned();
    stranger.tracks[1].artists = vec![CreditedArtist {
        id: "0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b".to_owned(),
        name: "Someone Else".to_owned(),
        sort_name: None,
        join: String::new(),
    }];
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone(), other.clone(), stranger],
        ..RecallResult::default()
    });
    let (status, body) = app
        .call(
            "GET",
            &format!("{plan}?release_mbid={STRANGER}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (&body["other_version_count"], &body["extra_count"]),
        (&json!(0), &json!(1)),
        "{body}"
    );
    assert_eq!(body["same_album"], json!(false), "{body}");

    let user = app.login_user("plain").await;
    let user_headers = [("authorization", user.authorization.as_str())];
    let (status, body) = app.call("GET", &plan, &user_headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["kept_count"], json!(2), "{body}");
    let (status, body) = app
        .call(
            "GET",
            &format!("{plan}?release_mbid={OTHER}"),
            &user_headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let search = format!("/api/v3/library/albums/{album}/reidentification/releases?title=Album");
    let (status, body) = app.call("GET", &search, &user_headers, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let trusted = app.login_user("tris").await;
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/admin/users/{}/role", trusted.user_id),
            &headers,
            Some(json!({"role": "trusted"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "GET",
            &search,
            &[("authorization", trusted.authorization.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.stop().await;
}

/// Another version pairs one file with one track: when two files (a 4:11
/// and a 6:02 cut, say) could both be another version of the same missing
/// track, neither is, and both stay files the edition has no track for.
#[tokio::test]
async fn edition_plan_pairs_another_version_one_to_one() {
    const OTHER: &str = "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f";
    let rig = Rig::start("lib-edition-plan-pairs").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-v/01.flac", "flac_full_01.flac");
    plant(&music, "album-v/02.flac", "flac_full_02.flac");
    plant(&music, "album-v/03.flac", "flac_full_02.flac");
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let tracks: Vec<String> = ["01", "02", "03"]
        .iter()
        .map(|name| {
            rig.scan_store
                .track_at("music", &format!("album-v/{name}.flac"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album = rig
        .scan_store
        .album_for_track(&tracks[0])
        .unwrap()
        .expect("album");
    for track in &tracks {
        assert_eq!(
            rig.scan_store.album_for_track(track).unwrap().as_ref(),
            Some(&album)
        );
    }
    let mut other = tagged_release(&rig.identify_store, &album);
    let mut seen = std::collections::HashSet::new();
    other
        .tracks
        .retain(|track| seen.insert(track.title.clone()));
    assert_eq!(other.tracks.len(), 2, "one track per song");
    other.id = OTHER.to_owned();
    let second = other
        .tracks
        .iter_mut()
        .find(|track| track.position == 2)
        .expect("the second track");
    second.id = "other-track-2".to_owned();
    second.recording_id = "b2c3d4e5-0000-4000-8000-000000000002".to_owned();
    second.length_ms = second.length_ms.map(|length| length + 45_000);
    rig.identify.set_recall(RecallResult {
        releases: vec![other],
        ..RecallResult::default()
    });
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album}/edition/plan?release_mbid={OTHER}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            &body["kept_count"],
            &body["missing_count"],
            &body["other_version_count"],
            &body["extra_count"],
        ),
        (&json!(1), &json!(1), &json!(0), &json!(2)),
        "{body}"
    );
    rig.stop().await;
}

/// Catalog corrections end to end: split a track off, find it still there
/// after a rescan, reset it back to the original album, merge a copy in
/// with its chosen edition following it, and merge a duplicate artist.
/// Applies refuse a token that does not match, and a replayed key returns
/// the first result. After a merge, the old album and artist ids still open
/// the survivor.
#[tokio::test]
async fn library_journey_catalog_corrections() {
    let rig = Rig::start("lib-corrections").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "a/02.flac", "flac_full_02.flac");
    plant(&music, "b/01.flac", "flac_full_01.flac");
    // Nothing is scripted at MusicBrainz, so identification stays paused:
    // the journey is about what a person does with the album.
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let scan = || async {
        app.scan_library().await;
    };
    scan().await;
    let store = &rig.scan_store;
    let a1 = store.track_at("music", "a/01.flac").unwrap().expect("a/01");
    let a2 = store.track_at("music", "a/02.flac").unwrap().expect("a/02");
    let b1 = store.track_at("music", "b/01.flac").unwrap().expect("b/01");
    let album_a = store.album_for_track(&a1).unwrap().expect("album a");
    let album_b = store.album_for_track(&b1).unwrap().expect("album b");
    assert_ne!(album_a, album_b, "copies in two folders are two albums");

    // Split track 2 off album A.
    let selection = json!({"track_ids": [a2], "title": "Bonus"});
    let (status, preview) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album_a}/split-preview"),
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["automatic_groups"][0]["created"], json!(true));
    assert_eq!(preview["automatic_groups"][0]["title"], json!("Bonus"));
    assert_eq!(
        store.album_for_track(&a2).unwrap().as_deref(),
        Some(album_a.as_str()),
        "a preview changes nothing"
    );
    let token = preview["preview_token"].as_str().expect("token").to_owned();
    let split = format!("/api/v3/library/albums/{album_a}/split");
    let mut apply = selection.clone();
    apply["preview_token"] = json!("1.0000");
    let (status, body) = app.call("POST", &split, &headers, Some(apply)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("PREVIEW_EXPIRED"));
    let mut apply = selection.clone();
    apply["preview_token"] = json!(token);
    apply["idempotency_key"] = json!("split-1");
    let (status, applied) = app
        .call("POST", &split, &headers, Some(apply.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let album_new = applied["target_album_id"]
        .as_str()
        .expect("new album")
        .to_owned();
    assert_eq!(
        store.album_for_track(&a2).unwrap().as_deref(),
        Some(album_new.as_str())
    );
    let (status, replay) = app.call("POST", &split, &headers, Some(apply)).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["target_album_id"], json!(album_new));

    // A rescan keeps the manual grouping.
    scan().await;
    assert_eq!(
        store.album_for_track(&a2).unwrap().as_deref(),
        Some(album_new.as_str())
    );

    // Reset grouping sends the track back to album A; the split album is
    // retired into it.
    let selection = json!({"track_ids": [a2]});
    let (status, preview) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album_new}/reset-grouping-preview"),
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(
        preview["automatic_groups"][0]["local_album_id"],
        json!(album_a)
    );
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album_new}/reset-grouping"),
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        store.album_for_track(&a2).unwrap().as_deref(),
        Some(album_a.as_str())
    );
    let db = rusqlite::Connection::open(&app.config.library_db_path).expect("db opens");
    let retired_into = |album: &str| -> Option<String> {
        db.query_row(
            "SELECT retired_into_album_id FROM local_albums WHERE id = ?1",
            [album],
            |row| row.get(0),
        )
        .expect("album row")
    };
    assert_eq!(retired_into(&album_new).as_deref(), Some(album_a.as_str()));

    // Album B carries a chosen edition, album A none: merging B into A
    // moves the edition onto A.
    const CHOSEN: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    db.execute_batch(&format!(
        "DELETE FROM local_track_external_identities; \
         DELETE FROM local_album_external_identities; \
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
         release_mbid, decision_source, selected_at) VALUES ('{album_b}', \
         '11111111-2222-4333-8444-555555555555', '{CHOSEN}', 'manual', 1);"
    ))
    .expect("pin album b");
    // Someone favorited album B and played it; both follow the merge.
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'album', '{album_b}', 1); \
         INSERT INTO library_user_favorite_names (user_id, item_kind, item_id, display_name) \
         VALUES ('listener', 'album', '{album_b}', 'Album B'); \
         INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
         artist_name, played_at) VALUES ('play-1', 'listener', '{album_b}', 'Song', \
         'Artist', '2026-01-01T00:00:00Z');"
    ))
    .expect("favorite and play album b");
    let selection = json!({"track_ids": [b1], "target_album_id": album_a});
    let (status, preview) = app
        .call(
            "POST",
            "/api/v3/library/albums/merge-preview",
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["aliases"], json!([album_b]));
    assert_eq!(preview["edition_changes"][0]["change"], json!("moved"));
    // The preview was for "detach"; applying it as "retain" is refused.
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    let mut other_choice = apply.clone();
    other_choice["identity_choice"] = json!("retain_manual");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/albums/merge",
            &headers,
            Some(other_choice),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("STALE_REVISION"));
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/albums/merge",
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (favorite, played): (String, String) = db
        .query_row(
            "SELECT (SELECT item_id FROM library_user_favorite_names WHERE user_id = 'listener' \
             AND display_name = 'Album B'), \
             (SELECT local_album_id FROM library_play_history WHERE id = 'play-1')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("references");
    assert_eq!(
        (favorite.as_str(), played.as_str()),
        (album_a.as_str(), album_a.as_str())
    );
    assert_eq!(
        store.album_for_track(&b1).unwrap().as_deref(),
        Some(album_a.as_str())
    );
    assert_eq!(retired_into(&album_b).as_deref(), Some(album_a.as_str()));
    let pinned: String = db
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album_a],
            |row| row.get(0),
        )
        .expect("edition moved to album a");
    assert_eq!(pinned, CHOSEN);

    // The merged album's old id opens the survivor, with its tracks.
    let (status, survivor) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album_a}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{survivor}");
    let (status, old) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album_b}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert_eq!(old["id"], json!(album_a));
    assert_eq!(old["title"], survivor["title"]);
    let (status, tracks) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album_b}/tracks"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tracks}");
    let (_, survivor_tracks) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{album_a}/tracks"),
            &headers,
            None,
        )
        .await;
    assert_eq!(tracks, survivor_tracks);
    assert!(
        tracks["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|track| track["id"] == json!(b1)))
    );

    // Two artists that are the same person: merge one into the other.
    db.execute_batch(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist-keep', 'Same', 'same', 'person', 1, 1), \
         ('artist-dupe', 'Same.', 'same.', 'person', 1, 1); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('someone', 'artist', 'artist-dupe', 1);",
    )
    .expect("artists");
    let selection = json!({
        "source_artist_ids": ["artist-keep", "artist-dupe"],
        "surviving_artist_id": "artist-keep",
        "expected_revisions": {"artist-keep": 1, "artist-dupe": 1},
    });
    let (status, preview) = app
        .call(
            "POST",
            "/api/v3/library/artists/merge-preview",
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["reference_counts"]["favorites"], json!(1));
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    apply["idempotency_key"] = json!("artist-merge-1");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/artists/merge",
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["retired_artist_ids"], json!(["artist-dupe"]));
    let (retired, favorite): (Option<String>, String) = db
        .query_row(
            "SELECT (SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-dupe'), \
             (SELECT item_id FROM library_user_favorites WHERE user_id = 'someone')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("merged artist");
    assert_eq!(retired.as_deref(), Some("artist-keep"));
    assert_eq!(favorite, "artist-keep");

    // The merged artist's old id opens the survivor and its albums.
    let (status, old) = app
        .call("GET", "/api/v3/library/artists/artist-dupe", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert_eq!(old["id"], json!("artist-keep"));
    let (status, albums) = app
        .call(
            "GET",
            "/api/v3/library/artists/artist-dupe/albums",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{albums}");
    let (_, survivor_albums) = app
        .call(
            "GET",
            "/api/v3/library/artists/artist-keep/albums",
            &headers,
            None,
        )
        .await;
    assert_eq!(albums, survivor_albums);
    rig.stop().await;
}

/// Run one Library Management worker step off the async runtime.
/// One look by the automatic worker, on the blocking pool as in
/// production (it may wait for MusicBrainz).
async fn automatic_next(library: &LibrarySetup) -> bool {
    let management = droppedneedle_library::management::service::Management::new(library);
    tokio::task::spawn_blocking(move || management.automatic_next())
        .await
        .expect("worker joins")
        .expect("automatic step")
}

async fn management_step(library: &LibrarySetup) -> bool {
    let management = droppedneedle_library::management::service::Management::new(library);
    tokio::task::spawn_blocking(move || management.run_next("test-worker"))
        .await
        .expect("worker joins")
        .expect("worker step runs")
}

/// Make a ready preview and apply it; returns the job id once the worker
/// wrote it.
async fn management_run(
    library: &LibrarySetup,
    request: droppedneedle_library::management::service::PreviewRequest,
) -> String {
    use droppedneedle_library::management::service::{ApplyRequest, Management};
    let management = Management::new(library);
    let handle = management
        .create_preview(request, "admin")
        .expect("preview queued");
    assert!(management_step(library).await, "planning runs");
    let detail = management.detail(&handle.job_id).expect("detail");
    assert!(detail.ready_for_confirmation, "{:?}", detail.stale_reasons);
    management
        .apply(
            &handle.job_id,
            ApplyRequest {
                preview_token: handle.preview_token,
                expected_row_revision: detail.job.row_revision,
                idempotency_key: format!("apply-{}", handle.job_id),
                confirmation: true,
            },
        )
        .expect("apply queued");
    assert!(management_step(library).await, "apply runs");
    let applied = management.detail(&handle.job_id).expect("detail");
    assert_eq!(applied.job.state.as_str(), "succeeded");
    handle.job_id
}

/// A management step whose read finds the database busy puts its job
/// back in the queue as it stood, not failed, and the next step plans it.
#[tokio::test(flavor = "multi_thread")]
async fn a_busy_database_leaves_a_management_job_runnable() {
    use droppedneedle_kernel::db::{DbConfig, DbError, open_runtime};
    use droppedneedle_library::management::models::{ManagementError, Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;

    let scratch = Scratch {
        dir: ScratchDir::new("lib-management-busy"),
        identify: Arc::new(FakeProviders::default()),
        enrichment: None,
    };
    crate::common::migrated_db(&scratch.join("app.db"));
    // One request reader and a short wait for it: while a read holds it,
    // the worker's reads answer busy at once.
    let mut config = DbConfig::new(&scratch.join("app.db"))
        .with_read_wait(std::time::Duration::from_millis(100));
    config.readers = 1;
    let runtime = open_runtime(&config).await.expect("database opens");
    let settings_store = Arc::new(
        ConfigStore::scratch(
            &scratch,
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
        )
        .expect("config opens"),
    );
    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .settings(settings_store)
        .build()
        .await;
    let library = library_asking(&platform, scratch.identify.clone());
    let music = scratch.join("music");
    plant(&music, "incoming/busy.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/busy.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    runtime
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    let settings = library
        .settings
        .store()
        .save_blocking(lm::initial_settings(""))
        .expect("settings saved");
    let management = Management::new(&library);
    let handle = management
        .create_preview(
            PreviewRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album],
                    catalog_filter: None,
                },
                profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                expected_settings_revision: lm::settings_revision(&settings),
                expected_policy_revision: library.core.policy_revision().expect("policy revision"),
                idempotency_key: Some("preview-busy".to_owned()),
                target_root_id: None,
                overrides: None,
            },
            "admin",
        )
        .expect("preview queued");

    // The reader is held: the step claims the job, a write, and its first
    // read is busy.
    let (entered, inside) = std::sync::mpsc::channel();
    let (let_go, release) = std::sync::mpsc::channel::<()>();
    let held = {
        let db = runtime.db().clone();
        std::thread::spawn(move || {
            db.read_blocking("test.held", |_| {
                entered.send(()).expect("test waits");
                release.recv().expect("test lets go");
                Ok::<_, DbError>(())
            })
        })
    };
    inside.recv().expect("reader held");
    let step = Management::new(&library);
    let outcome = tokio::task::spawn_blocking(move || step.run_next("test-worker"))
        .await
        .expect("worker joins");
    let_go.send(()).expect("reader lets go");
    held.join().expect("reader joins").expect("held read");

    let job = management.detail(&handle.job_id).expect("detail").job;
    assert_eq!(job.state.as_str(), "queued", "{outcome:?}");
    assert!(
        matches!(outcome, Err(ManagementError::Busy(_))),
        "{outcome:?}"
    );
    assert!(management_step(&library).await, "planning runs");
    let job = management.detail(&handle.job_id).expect("detail").job;
    assert_eq!(job.state.as_str(), "ready");
    runtime.shutdown().await;
}

/// A management job whose worker died mid-run, its lease run out, is
/// requeued and finished by the management worker alone: no other
/// feature's loop has to notice it.
#[tokio::test]
async fn a_management_job_whose_worker_died_is_finished_by_its_own_worker() {
    use droppedneedle_library::management::models::{Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;

    let (_scratch, library, music) = bare_library("lib-management-dead-worker").await;
    plant(&music, "incoming/dead.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/dead.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    let settings = library
        .settings
        .store()
        .save_blocking(lm::initial_settings(""))
        .expect("settings saved");
    let management = Management::new(&library);
    let handle = management
        .create_preview(
            PreviewRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album],
                    catalog_filter: None,
                },
                profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                expected_settings_revision: lm::settings_revision(&settings),
                expected_policy_revision: library.core.policy_revision().expect("policy revision"),
                idempotency_key: Some("preview-dead-worker".to_owned()),
                target_root_id: None,
                overrides: None,
            },
            "admin",
        )
        .expect("preview queued");

    // A worker took the job and died: the row says running, under a
    // lease that ran out a minute ago.
    library
        .scan_store
        .db()
        .run_sql(&format!(
            "UPDATE library_operation_jobs SET state = 'running', lease_owner = 'gone', \
             lease_expires_at = unixepoch('now') - 60, heartbeat_at = unixepoch('now') - 120 \
             WHERE id = '{}'",
            handle.job_id
        ))
        .expect("job held by a dead worker");

    assert!(management_step(&library).await, "the job is taken again");
    let job = management.detail(&handle.job_id).expect("detail").job;
    assert_eq!(job.state.as_str(), "ready", "planning finished");
}

/// The management worker on its own loop: its first turn requeues and
/// runs a job a stopped process left running, and a job queued later
/// starts on the wakeup its queueing sent. No other library loop runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_management_loop_recovers_at_start_and_wakes_on_enqueue() {
    use droppedneedle_library::jobs::MANAGEMENT_WAKEUP;
    use droppedneedle_library::management::models::{Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;

    let scratch = Scratch {
        dir: ScratchDir::new("lib-management-loop"),
        identify: Arc::new(FakeProviders::default()),
        enrichment: None,
    };
    crate::common::migrated_db(&scratch.join("app.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&scratch.join("app.db"))
        .expect("database opens");
    let platform = crate::common::TestPlatform::on(&db, &scratch)
        .settings(Arc::new(
            ConfigStore::scratch(
                &scratch,
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("config opens"),
        ))
        .build()
        .await;
    let library = library_with(&platform, scratch.identify.clone(), None);
    let music = scratch.join("music");
    plant(&music, "incoming/loop.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/loop.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    db.run_sql(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
    )
    .expect("admin row");
    let settings = library
        .settings
        .store()
        .save_blocking(lm::initial_settings(""))
        .expect("settings saved");
    let management = Management::new(&library);
    let preview = |key: &str| {
        management
            .create_preview(
                PreviewRequest {
                    selection: Selection {
                        kind: SelectionKind::Albums,
                        ids: vec![album.clone()],
                        catalog_filter: None,
                    },
                    profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                    expected_settings_revision: lm::settings_revision(&settings),
                    expected_policy_revision: library
                        .core
                        .policy_revision()
                        .expect("policy revision"),
                    idempotency_key: Some(key.to_owned()),
                    target_root_id: None,
                    overrides: None,
                },
                "admin",
            )
            .expect("preview queued")
            .job_id
    };
    // Waits on the job's own state, woken by each commit; the bound only
    // stops a hang.
    let settled = |job_id: String| {
        let (management, mut commits) = (management.clone(), db.commits());
        async move {
            let wait = async {
                loop {
                    let job_id = job_id.clone();
                    let step = management.clone();
                    let state = tokio::task::spawn_blocking(move || {
                        step.detail(&job_id).expect("detail").job.state
                    })
                    .await
                    .expect("detail joins");
                    if state.as_str() == "ready" {
                        return;
                    }
                    commits.changed().await.expect("database open");
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(60), wait)
                .await
                .expect("the job reached ready");
        }
    };

    // A process held the job and stopped before its lease ran out.
    let interrupted = preview("preview-interrupted");
    db.run_sql(&format!(
        "UPDATE library_operation_jobs SET state = 'running', lease_owner = 'stopped', \
         lease_expires_at = unixepoch('now') + 600, heartbeat_at = unixepoch('now') \
         WHERE id = '{interrupted}'"
    ))
    .expect("job held by a stopped process");

    droppedneedle_library::management::start(
        droppedneedle_library::management::service::Management::new(&library),
        &platform.supervisor,
    )
    .expect("management loop starts");
    settled(interrupted).await;

    let woken_at = library.wakeups.revision(MANAGEMENT_WAKEUP);
    let queued = preview("preview-queued");
    assert_ne!(
        library.wakeups.revision(MANAGEMENT_WAKEUP),
        woken_at,
        "queueing the job woke the worker"
    );
    settled(queued).await;

    assert_eq!(
        platform.supervisor.running_names(),
        vec![droppedneedle_library::management::JOB_NAME.to_owned()]
    );
    platform
        .supervisor
        .shutdown(std::time::Duration::from_secs(5))
        .await;
}

/// The repairs worker on its own loop: its first turn requeues and
/// finishes a repair job a stopped process left running, and a job queued
/// later starts on the wakeup its queueing sent. No other library loop
/// runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_repairs_loop_recovers_at_start_and_wakes_on_enqueue() {
    use droppedneedle_library::jobs::REPAIRS_WAKEUP;
    use droppedneedle_library::repairs::models::Purpose;

    let scratch = Scratch {
        dir: ScratchDir::new("lib-repairs-loop"),
        identify: Arc::new(FakeProviders::default()),
        enrichment: None,
    };
    crate::common::migrated_db(&scratch.join("app.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&scratch.join("app.db"))
        .expect("database opens");
    let platform = crate::common::TestPlatform::on(&db, &scratch)
        .settings(Arc::new(
            ConfigStore::scratch(
                &scratch,
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("config opens"),
        ))
        .build()
        .await;
    let library = library_with(&platform, scratch.identify.clone(), None);
    let repairs = crate::common::library::services(&library).repairs;
    // A recheck of an empty library: its report is ready once it runs.
    let check = |key: &str| {
        repairs
            .create(
                Purpose::ExistingMatches,
                "admin",
                key,
                Vec::new(),
                None,
                "test-matcher".to_owned(),
            )
            .expect("check queued")
            .id
    };
    // Waits on the job's own state, woken by each commit; the bound only
    // stops a hang.
    let settled = |job_id: String| {
        let (repairs, mut commits) = (repairs.clone(), db.commits());
        async move {
            let wait = async {
                loop {
                    let job_id = job_id.clone();
                    let read = repairs.clone();
                    let state = tokio::task::spawn_blocking(move || {
                        read.get(&job_id, Purpose::ExistingMatches)
                            .expect("detail")
                            .detail
                            .job
                            .state
                    })
                    .await
                    .expect("detail joins");
                    if state.as_str() == "ready" {
                        return;
                    }
                    commits.changed().await.expect("database open");
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(60), wait)
                .await
                .expect("the report is ready");
        }
    };

    // A process held the job and stopped before its lease ran out.
    let interrupted = check("repair-interrupted");
    db.run_sql(&format!(
        "UPDATE library_operation_jobs SET state = 'running', lease_owner = 'stopped', \
         lease_expires_at = unixepoch('now') + 600, heartbeat_at = unixepoch('now') \
         WHERE id = '{interrupted}'"
    ))
    .expect("job held by a stopped process");

    droppedneedle_library::repairs::start(
        crate::common::library::repair_worker(&library),
        &platform.supervisor,
    )
    .expect("repairs loop starts");
    settled(interrupted).await;

    let woken_at = library.wakeups.revision(REPAIRS_WAKEUP);
    let queued = check("repair-queued");
    assert_ne!(
        library.wakeups.revision(REPAIRS_WAKEUP),
        woken_at,
        "queueing the job woke the worker"
    );
    settled(queued).await;

    assert_eq!(
        platform.supervisor.running_names(),
        vec![droppedneedle_library::repairs::JOB_NAME.to_owned()]
    );
    platform
        .supervisor
        .shutdown(std::time::Duration::from_secs(5))
        .await;
}

/// A Library Management job end to end:
/// - an album whose edition is a best guess is left alone with its reason;
/// - once confirmed, picking one file still plans (and moves) the whole
///   album, and a rename that only changes letter case goes through;
/// - the Picard-style profile then retags and moves the album, its cover
///   and rip log travel with it, a wrong token is refused, and results
///   and history read back;
/// - an undo preview, applied, puts files and sidecars back with their
///   tags, and a second undo is refused.
#[tokio::test]
async fn library_management_preview_apply_undo() {
    use droppedneedle_kernel::runtime_config::sections::NamingScript;
    use droppedneedle_library::management::models::{ItemFilter, Selection, SelectionKind};
    use droppedneedle_library::management::service::{ApplyRequest, Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, music) = bare_library("lib-management-jobs").await;
    let first = plant(
        &music,
        "incoming/management track.flac",
        "management_full.flac",
    );
    let second = plant(&music, "incoming/second.flac", "management_full.flac");
    save_tags(
        &second,
        &[
            TagEdit::verbatim(TagField::Title, vec!["Second Song".to_owned()]),
            TagEdit::verbatim(TagField::TrackNumber, vec!["3".to_owned()]),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
        ],
    )
    .expect("second file retagged");
    std::fs::write(music.join("incoming/cover.jpg"), b"cover bytes").expect("cover");
    std::fs::write(music.join("incoming/rip.log"), b"rip log").expect("log");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/management track.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    // A second profile names files after their title in place, so the
    // first file's rename only changes letter case.
    let mut settings = lm::initial_settings("");
    settings.naming_scripts.push(NamingScript {
        id: "in-place".to_owned(),
        name: "In place".to_owned(),
        source: "incoming/{title}.{ext}".to_owned(),
        ..NamingScript::default()
    });
    let mut in_place = settings
        .profiles
        .iter()
        .find(|profile| profile.id == lm::PICARD_ORGANIZER_PROFILE_ID)
        .expect("picard preset")
        .clone();
    in_place.id = "in-place".to_owned();
    in_place.name = "In place".to_owned();
    in_place.organization.naming_script_id = "in-place".to_owned();
    in_place.organization.multi_disc_naming_script_id = None;
    settings.profiles.push(in_place);
    let settings = library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");

    let management = Management::new(&library);
    let request =
        |profile: &str, kind: SelectionKind, ids: Vec<String>, key: &str| PreviewRequest {
            selection: Selection {
                kind,
                ids,
                catalog_filter: None,
            },
            profile_id: profile.to_owned(),
            expected_settings_revision: lm::settings_revision(&settings),
            expected_policy_revision: library.core.policy_revision().expect("policy revision"),
            idempotency_key: Some(key.to_owned()),
            target_root_id: None,
            overrides: None,
        };

    // The tie was decided as a best guess: its files wait, and say why.
    let guessed = management
        .create_preview(
            request(
                lm::PICARD_ORGANIZER_PROFILE_ID,
                SelectionKind::Albums,
                vec![album.clone()],
                "preview-guess",
            ),
            "admin",
        )
        .expect("preview queued");
    assert!(management_step(&library).await, "planning runs");
    let page = management
        .items(&guessed.job_id, -1, 100, &ItemFilter::default())
        .expect("items page");
    let reason = page.items[0].facts.reason.clone().expect("a reason");
    assert_eq!(reason.code, "UNCONFIRMED_MATCH");
    assert!(!reason.action.is_empty());
    crate::common::library::operations(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");

    // One picked file moves its whole album; the case-only rename is the
    // file itself, not a collision.
    let in_place_job = management_run(
        &library,
        request(
            "in-place",
            SelectionKind::Tracks,
            vec![track.clone()],
            "preview-in-place",
        ),
    )
    .await;
    let detail = management.detail(&in_place_job).expect("detail");
    assert_eq!(detail.summary.selected_item_count, Some(1));
    assert_eq!(detail.summary.item_count, 2, "the whole album is planned");
    assert!(music.join("incoming/Management Track.flac").exists());
    assert!(music.join("incoming/Second Song.flac").exists());
    assert!(!first.exists() && !second.exists());
    let renamed = music.join("incoming/Management Track.flac");
    let tagged = read_fields(&renamed).expect("fields read");

    // The Picard-style profile moves the album; its sidecars come along.
    let handle = management
        .create_preview(
            request(
                lm::PICARD_ORGANIZER_PROFILE_ID,
                SelectionKind::Albums,
                vec![album.clone()],
                "preview-picard",
            ),
            "admin",
        )
        .expect("preview queued");
    assert!(management_step(&library).await, "planning runs");
    let detail = management.detail(&handle.job_id).expect("detail");
    assert!(detail.ready_for_confirmation, "{:?}", detail.stale_reasons);
    let page = management
        .items(&handle.job_id, -1, 100, &ItemFilter::default())
        .expect("items page");
    let item = page
        .items
        .iter()
        .find(|item| item.source_rel == "incoming/Management Track.flac")
        .expect("planned file");
    assert!(item.eligibility.writes(), "{:?}", item.facts.reason);
    let dest = item.dest_rel.clone().expect("destination planned");
    let dest_dir = std::path::Path::new(&dest)
        .parent()
        .expect("destination folder")
        .to_path_buf();
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.diff.sidecars.len())
            .sum::<usize>(),
        2,
        "cover and log are planned"
    );
    let wrong = management.apply(
        &handle.job_id,
        ApplyRequest {
            preview_token: "not-the-token".to_owned(),
            expected_row_revision: detail.job.row_revision,
            idempotency_key: "apply-1".to_owned(),
            confirmation: true,
        },
    );
    assert!(wrong.is_err());
    management
        .apply(
            &handle.job_id,
            ApplyRequest {
                preview_token: handle.preview_token.clone(),
                expected_row_revision: detail.job.row_revision,
                idempotency_key: "apply-1".to_owned(),
                confirmation: true,
            },
        )
        .expect("apply queued");
    assert!(management_step(&library).await, "apply runs");
    let applied = management.detail(&handle.job_id).expect("detail");
    assert_eq!(applied.job.state.as_str(), "succeeded");
    assert!(!renamed.exists(), "the source moved");
    assert!(music.join(&dest).exists(), "the file sits at its new path");
    assert!(music.join(&dest_dir).join("cover.jpg").exists());
    assert!(music.join(&dest_dir).join("rip.log").exists());
    assert!(!music.join("incoming/cover.jpg").exists());
    let results = management
        .results(&handle.job_id, -1, 100)
        .expect("results");
    assert!(
        results
            .items
            .iter()
            .all(|item| item.work_state == ResultState::Succeeded)
    );
    assert!(applied.undo_available_count >= 1);

    // Undo puts files, tags and sidecars back; a second undo is refused.
    let undo = management
        .undo_preview(&handle.job_id, applied.job.row_revision, "undo-1", "admin")
        .expect("undo preview queued");
    assert!(management_step(&library).await, "undo planning runs");
    let undo_detail = management.detail(&undo.job_id).expect("undo detail");
    assert!(undo_detail.ready_for_confirmation);
    management
        .apply(
            &undo.job_id,
            ApplyRequest {
                preview_token: undo.preview_token.clone(),
                expected_row_revision: undo_detail.job.row_revision,
                idempotency_key: "undo-apply-1".to_owned(),
                confirmation: true,
            },
        )
        .expect("undo queued");
    assert!(management_step(&library).await, "undo runs");
    let undone = management.detail(&undo.job_id).expect("detail");
    assert_eq!(undone.job.state.as_str(), "succeeded");
    assert_eq!(
        read_fields(&renamed).expect("restored file reads"),
        tagged,
        "undo puts every tag back"
    );
    assert_eq!(
        std::fs::read(music.join("incoming/cover.jpg")).expect("cover back"),
        b"cover bytes"
    );
    assert!(music.join("incoming/rip.log").exists());
    let source = management.detail(&handle.job_id).expect("detail");
    assert!(
        management
            .undo_preview(&handle.job_id, source.job.row_revision, "undo-2", "admin")
            .is_err(),
        "an undone run cannot be undone twice"
    );
    let (history, _) = management
        .history(Default::default())
        .expect("history reads");
    assert_eq!(history.len(), 4);
}

/// Run repair turns until the job leaves `queued` and `running`.
async fn settle_repair(app: &TestApp, headers: &[(&str, &str)], path: &str) -> Value {
    app.until("the repair job settles", || async {
        let (status, body) = app.call("GET", path, headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (!matches!(body["state"].as_str(), Some("queued" | "running"))).then_some(body)
    })
    .await
}

#[tokio::test]
async fn library_journey_identity_repair_and_preparation() {
    let rig = Rig::start("lib-repairs").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-r/01.flac", "flac_full_01.flac");
    plant(&music, "album-r/02.flac", "flac_full_02.flac");
    // Identification waits until the journey has scripted MusicBrainz for
    // the album the scan finds.
    let paused_at = rig.pause_identification().await;
    let run_id = app.add_library_folder("music", &music).await;
    let run_path = format!("/api/v3/library/scan/runs/{run_id}");
    let track_id = {
        rig.scan_store
            .track_at("music", "album-r/01.flac")
            .unwrap()
            .expect("track indexed")
    };
    let track_path = format!("/api/v3/library/tracks/{track_id}");
    let (_, body) = app.call("GET", &track_path, &headers, None).await;
    let album = body["album_id"].as_str().expect("album id").to_owned();
    let release = tagged_release(&rig.identify_store, &album);
    let scripted = &rig.identify;
    scripted.set_recall(RecallResult {
        releases: vec![release.clone()],
        ..RecallResult::default()
    });
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;

    // Scan diagnostics: a redacted download, never a full path.
    let (status, body) = app
        .call("GET", &format!("{run_path}/diagnostics"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["format"], json!("droppedneedle-library-diagnostic-v1"));
    assert!(!body.to_string().contains(&*music.to_string_lossy()));
    let (status, _) = app
        .call(
            "GET",
            "/api/v3/library/scan/runs/nope/diagnostics",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A recheck while the match holds: one album, valid, nothing to apply.
    let (_, body) = app
        .call(
            "GET",
            "/api/v3/library/identity-repairs/estimate",
            &headers,
            None,
        )
        .await;
    assert_eq!(body["identity_count"], json!(1), "{body}");
    let create = json!({"idempotency_key": "repair-1"});
    let (status, job) = app
        .call(
            "POST",
            "/api/v3/library/identity-repairs",
            &headers,
            Some(create.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let (_, again) = app
        .call(
            "POST",
            "/api/v3/library/identity-repairs",
            &headers,
            Some(create),
        )
        .await;
    assert_eq!(again["id"], job["id"], "same key, same job");
    let job_path = format!(
        "/api/v3/library/identity-repairs/{}",
        job["id"].as_str().expect("id")
    );
    let ready = settle_repair(app, &headers, &job_path).await;
    assert_eq!(ready["state"], json!("ready"), "{ready}");
    assert_eq!(
        ready["repair_summary"]["counts_by_finding"]["valid"],
        json!(1)
    );
    let findings_path = format!("{job_path}/findings");
    let (_, findings) = app.call("GET", &findings_path, &headers, None).await;
    let item = &findings["items"][0];
    assert_eq!(item["reason_code"], json!("CURRENT_IDENTITY_PASSES"));
    assert!(item["reason"]["action"].is_string(), "{findings}");

    // MusicBrainz now lists other recordings on the release: no file fits
    // it any more, so the recheck offers to detach the match.
    let mut moved = release.clone();
    for (index, track) in moved.tracks.iter_mut().enumerate() {
        track.recording_id = format!("00000000-0000-4000-8000-00000000000{index}");
        track.title = format!("Something else {index}");
        track.length_ms = Some(999_000);
    }
    scripted.set_recall(RecallResult {
        releases: vec![moved],
        ..RecallResult::default()
    });
    let (_, job) = app
        .call(
            "POST",
            "/api/v3/library/identity-repairs",
            &headers,
            Some(json!({"idempotency_key": "repair-2"})),
        )
        .await;
    let job_path = format!(
        "/api/v3/library/identity-repairs/{}",
        job["id"].as_str().expect("id")
    );
    let ready = settle_repair(app, &headers, &job_path).await;
    assert_eq!(
        ready["repair_summary"]["estimated_apply_changes"],
        json!(1),
        "{ready}"
    );
    let apply = format!("{job_path}/apply");
    let revision = ready["row_revision"].clone();
    let unconfirmed = json!({"expected_row_revision": revision, "confirmation": false});
    let (status, _) = app.call("POST", &apply, &headers, Some(unconfirmed)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let stale = json!({"expected_row_revision": 0, "confirmation": true});
    let (status, _) = app.call("POST", &apply, &headers, Some(stale)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let confirmed = json!({"expected_row_revision": revision, "confirmation": true});
    let (status, body) = app.call("POST", &apply, &headers, Some(confirmed)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done = settle_repair(app, &headers, &job_path).await;
    assert_eq!(done["state"], json!("succeeded"), "{done}");
    assert_eq!(done["succeeded_count"], json!(1));
    assert!(rig.identify_store.album_identity(&album).unwrap().is_none());
    let (_, findings) = app
        .call("GET", &format!("{job_path}/findings"), &headers, None)
        .await;
    assert_eq!(findings["items"][0]["apply_result"], json!("DETACHED"));

    // The detach is undone from the album, like an edition change.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album}/edition/undo"),
            &headers,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let restored = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("restored");
    assert_eq!(restored.release_mbid.as_deref(), Some(release.id.as_str()));

    // Preparation: the album has its exact release, so the report is
    // ready or offers to place the files. Dismissing it changes nothing.
    scripted.set_recall(RecallResult {
        releases: vec![release.clone()],
        ..RecallResult::default()
    });
    let (_, estimate) = app
        .call(
            "GET",
            "/api/v3/library/management/identity-preparations/estimate?root_id=music",
            &headers,
            None,
        )
        .await;
    assert_eq!(estimate["album_count"], json!(1), "{estimate}");
    assert_eq!(estimate["selected_root_count"], json!(1));
    let (_, job) = app
        .call(
            "POST",
            "/api/v3/library/management/identity-preparations",
            &headers,
            Some(json!({"idempotency_key": "prep-1"})),
        )
        .await;
    let prep_path = format!(
        "/api/v3/library/management/identity-preparations/{}",
        job["id"].as_str().expect("id")
    );
    let as_preparation = job_path.replace("identity-repairs", "management/identity-preparations");
    let (status, _) = app.call("GET", &as_preparation, &headers, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a repair is not a preparation"
    );
    let ready = settle_repair(app, &headers, &prep_path).await;
    assert_eq!(ready["state"], json!("ready"), "{ready}");
    let (_, findings) = app
        .call("GET", &format!("{prep_path}/findings"), &headers, None)
        .await;
    let code = findings["items"][0]["finding_code"]
        .as_str()
        .expect("finding");
    assert!(matches!(code, "ready" | "mapping_ready"), "{findings}");
    let (status, body) = app
        .call(
            "POST",
            &format!("{prep_path}/discard"),
            &headers,
            Some(json!({"expected_row_revision": ready["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["terminal_code"],
        json!("IDENTITY_PREPARATION_DISCARDED")
    );
    let (_, list) = app
        .call(
            "GET",
            "/api/v3/library/management/identity-preparations?limit=5",
            &headers,
            None,
        )
        .await;
    assert_eq!(list["items"].as_array().map(Vec::len), Some(1), "{list}");

    // Automatic edition acceptance: with the profile switch on, an album
    // without an exact edition gets the one edition that clearly fits,
    // as an unconfirmed automatic match with its undo.
    {
        use droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile;
        let page = "/api/v3/settings/library-management";
        let (status, saved) = app.call("GET", page, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let mut settings = saved.clone();
        settings
            .as_object_mut()
            .expect("settings")
            .remove("settings_revision");
        let mut profile = LibraryManagementProfile {
            id: "7c0f2f52-3a41-4d0e-9c57-2b1f4c6a8e10".to_owned(),
            name: "Auto".to_owned(),
            ..LibraryManagementProfile::default()
        };
        profile.identity.automatic_edition_acceptance_enabled = true;
        settings["profiles"]
            .as_array_mut()
            .expect("profiles")
            .push(serde_json::to_value(profile).expect("profile serializes"));
        settings["default_profile_id"] = json!("7c0f2f52-3a41-4d0e-9c57-2b1f4c6a8e10");
        let (status, body) = app
            .call(
                "PUT",
                page,
                &headers,
                Some(json!({
                    "settings": settings,
                    "expected_settings_revision": saved["settings_revision"],
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let conn = droppedneedle_kernel::db::open_connection(&app.config.library_db_path)
            .expect("db opens");
        conn.execute(
            "DELETE FROM local_track_external_identities WHERE local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            [&album],
        )
        .expect("track rows cleared");
        conn.execute(
            "DELETE FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
        )
        .expect("album identity cleared");
        conn.execute(
            "INSERT OR REPLACE INTO library_album_match_state (local_album_id, state, \
             reason_code, candidates_json, updated_at) \
             VALUES (?1, 'unmatched', 'NO_CANDIDATE', '[]', 1.0)",
            [&album],
        )
        .expect("match state planted");
    }
    let (_, job) = app
        .call(
            "POST",
            "/api/v3/library/management/identity-preparations",
            &headers,
            Some(json!({"idempotency_key": "prep-auto"})),
        )
        .await;
    let auto_path = format!(
        "/api/v3/library/management/identity-preparations/{}",
        job["id"].as_str().expect("id")
    );
    let ready = settle_repair(app, &headers, &auto_path).await;
    assert_eq!(ready["state"], json!("ready"), "{ready}");
    let (_, findings) = app
        .call("GET", &format!("{auto_path}/findings"), &headers, None)
        .await;
    let item = &findings["items"][0];
    assert_eq!(
        item["apply_result"],
        json!("EDITION_AUTO_ACCEPTED"),
        "{findings}"
    );
    assert_eq!(item["suggested_edition"]["auto_gate"], json!("AUTO_ACCEPT"));
    let accepted = rig
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("accepted");
    assert_eq!(accepted.release_mbid.as_deref(), Some(release.id.as_str()));
    // A confident acceptance is applied as confirmed: no flag remains.
    let match_state = || -> Option<String> {
        use rusqlite::OptionalExtension as _;
        droppedneedle_kernel::db::open_connection(&app.config.library_db_path)
            .expect("db opens")
            .query_row(
                "SELECT state || ':' || reason_code FROM library_album_match_state \
                 WHERE local_album_id = ?1",
                [&album],
                |row| row.get(0),
            )
            .optional()
            .expect("match state")
    };
    assert_eq!(match_state(), None);
    let undo = &item["automatic_undo"];
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album}/undo-automatic-edition"),
            &headers,
            Some(json!({
                "expected_album_revision": undo["expected_album_revision"],
                "expected_identity_revision": undo["expected_identity_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(rig.identify_store.album_identity(&album).unwrap().is_none());
    // The undo puts back the match state the acceptance replaced.
    assert_eq!(match_state().as_deref(), Some("unmatched:NO_CANDIDATE"));
    rig.stop().await;
}

/// Apply a ready preview from its handle and run the worker once.
async fn management_apply(
    library: &LibrarySetup,
    handle: &droppedneedle_library::management::models::PreviewHandle,
) -> droppedneedle_library::management::models::JobDetail {
    use droppedneedle_library::management::service::{ApplyRequest, Management};
    let management = Management::new(library);
    let detail = management.detail(&handle.job_id).expect("detail");
    assert!(detail.ready_for_confirmation, "{:?}", detail.stale_reasons);
    let job = management
        .apply(
            &handle.job_id,
            ApplyRequest {
                preview_token: handle.preview_token.clone(),
                expected_row_revision: detail.job.row_revision,
                idempotency_key: format!("apply-{}", handle.job_id),
                confirmation: true,
            },
        )
        .expect("apply queued");
    // Other queued work may go first; run the worker until this job ends.
    let mut state = job.state.as_str().to_owned();
    for _ in 0..5 {
        if !matches!(state.as_str(), "queued" | "running") {
            break;
        }
        if !management_step(library).await {
            let results = management.results(&handle.job_id, -1, 50).expect("results");
            panic!(
                "apply stuck at {state}: {:?}",
                results
                    .items
                    .iter()
                    .map(|item| (item.work_state, item.failure.clone()))
                    .collect::<Vec<_>>()
            );
        }
        state = management
            .detail(&handle.job_id)
            .expect("detail")
            .job
            .state
            .as_str()
            .to_owned();
    }
    management.detail(&handle.job_id).expect("detail")
}

/// Library Management tools end to end, every write through the
/// publisher:
/// - the tag editor shows a file's tags; a saved edit writes the file and
///   keeps the value, and its undo puts the tag back despite the kept
///   value it saved;
/// - a reset that changes no tag only drops the kept value;
/// - a baseline restore puts the file back to its original, and the
///   purge then drops the originals after a typed confirmation;
/// - the journal reports nothing needing attention;
/// - a dry run, confirmed, turns automatic management on: a queued album
///   is then managed without asking, and looked at again it is left
///   alone because it is already managed.
#[tokio::test]
async fn library_management_tools_journey() {
    use droppedneedle_library::management::activation::{
        ActivationConfirm, ActivationProof, ActivationRequest,
    };
    use droppedneedle_library::management::models::{Selection, SelectionKind, TagEditMode};
    use droppedneedle_library::management::restore::{PurgeRequest, RestoreRequest};
    use droppedneedle_library::management::service::Management;
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::management::tag_edit::TagEditRequest;
    use droppedneedle_library::tags::{TagField, read_fields};

    use droppedneedle_library::management::enrich::{Cover, LyricsFound, ScriptedEnrichment};
    let cover = {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        bytes
    };
    let synced = "[00:01.00]Plain words\n".to_owned();
    let (scratch, library, music) = bare_library_with(
        "lib-management-tools",
        Some(Arc::new(ScriptedEnrichment {
            listenbrainz: None,
            lyrics: Some(LyricsFound {
                plain: Some("Plain words".to_owned()),
                synced: Some(synced.clone()),
            }),
            cover: Some(Cover {
                bytes: cover.clone(),
                mime: "image/png".to_owned(),
            }),
            ..Default::default()
        })),
    )
    .await;
    let file = plant(&music, "incoming/tool track.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/tool track.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    crate::common::library::operations(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    // No MusicBrainz credits are scripted here, so the profile writes none.
    let mut settings = lm::initial_settings("");
    for profile in &mut settings.profiles {
        for field in &mut profile.metadata.fields {
            if droppedneedle_library::management::enrich::CREDIT_FIELDS
                .iter()
                .any(|(name, _)| *name == field.field)
            {
                field.mode = droppedneedle_kernel::runtime_config::sections::FieldMode::Disabled;
            }
        }
    }
    library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");
    // The settings page reads them first, which stores them normalized.
    lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
        .get_settings()
        .expect("settings normalized");
    let original = read_fields(&file).expect("original tags");
    let title =
        |path: &std::path::Path| read_fields(path).expect("tags read")[&TagField::Title].clone();
    let management = Management::new(&library);
    let policy = library.core.policy_revision().expect("policy revision");
    let revision = || {
        lm::settings_revision(
            &library
                .settings
                .store()
                .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
                .expect("settings read"),
        )
    };
    let overrides = |_: &LibrarySetup| -> i64 {
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db opens")
            .query_row(
                "SELECT COUNT(*) FROM library_management_overrides",
                [],
                |row| row.get(0),
            )
            .expect("overrides counted")
    };

    // Art, lyrics, ReplayGain and a scrub land through the publisher and
    // all come off again with the run's undo.
    {
        use droppedneedle_kernel::runtime_config::sections::{LibraryManagement, ReplayGainMode};
        use droppedneedle_library::tags::save::read_document;
        let db = || {
            droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
                .expect("db")
        };
        droppedneedle_library::tags::save_tags(
            &file,
            &[droppedneedle_library::tags::TagEdit::native(
                "vorbis:MOOD",
                vec!["Calm".to_owned()],
            )],
        )
        .expect("custom tag planted");
        rescan(&library).await;
        let before = read_document(&file).expect("tags before");
        let stored: LibraryManagement = library.settings.store().get().expect("settings");
        let mut enriched = stored.clone();
        for profile in enriched
            .profiles
            .iter_mut()
            .filter(|profile| profile.id == lm::PICARD_ORGANIZER_PROFILE_ID)
        {
            profile.enrichment.lyrics.enabled = true;
            profile.enrichment.lyrics.write_plain = true;
            profile.enrichment.lyrics.write_synced = true;
            profile.enrichment.replaygain.enabled = true;
            profile.enrichment.replaygain.mode = ReplayGainMode::Replace;
            profile.enrichment.replaygain.album_aware = true;
            profile.artwork.embedded_enabled = true;
            profile.artwork.external_enabled = true;
            profile.metadata.scrub_unmanaged_tags = true;
        }
        library
            .settings
            .store()
            .save_blocking(enriched)
            .expect("profile saved");
        let normalize = || {
            lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
                .get_settings()
                .expect("settings normalized");
        };
        normalize();
        let run = management
            .create_preview(
                droppedneedle_library::management::service::PreviewRequest {
                    selection: Selection {
                        kind: SelectionKind::Albums,
                        ids: vec![album.clone()],
                        catalog_filter: None,
                    },
                    profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                    expected_settings_revision: revision(),
                    expected_policy_revision: policy.clone(),
                    idempotency_key: Some("parity".to_owned()),
                    target_root_id: None,
                    overrides: None,
                },
                "admin",
            )
            .expect("parity preview");
        assert!(management_step(&library).await, "parity planning runs");
        let planned = management
            .items(&run.job_id, -1, 10, &Default::default())
            .expect("parity items")
            .items;
        let item = planned.first().expect("one file");
        assert!(item.eligibility.writes(), "{:?}", item.facts);
        assert!(
            item.diff
                .scrubbed_raw_tags
                .iter()
                .any(|tag| tag.key == "vorbis:MOOD"),
            "the preview lists the scrub"
        );
        let art = item
            .desired
            .artwork
            .iter()
            .find(|choice| choice.output_kind == "external_art")
            .expect("a cover file is planned");
        let (shown, mime) = management
            .preview_artwork(&run.job_id, item.ordinal, &art.blob_sha256)
            .expect("the preview serves its artwork");
        assert_eq!((shown, mime.as_str()), (cover.clone(), "image/png"));
        let applied = management_apply(&library, &run).await;
        assert_eq!(applied.job.state.as_str(), "succeeded");
        let rel: String = db()
            .query_row(
                "SELECT relative_path FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("managed path");
        let managed = music.join(&rel);
        let after = read_document(&managed).expect("tags after");
        assert!(!after.native.contains_key("vorbis:MOOD"), "scrubbed");
        assert_eq!(
            after.values.get(&TagField::Lyrics),
            Some(&vec!["Plain words".to_owned()])
        );
        // The fixtures are digital silence: ReplayGain says so rather than
        // writing a made-up gain.
        assert!(
            item.facts
                .warnings
                .iter()
                .any(|warning| warning.contains("silent")),
            "{:?}",
            item.facts.warnings
        );
        assert!(!after.values.contains_key(&TagField::ReplayGainTrackGain));
        let front = after
            .pictures
            .iter()
            .find(|picture| picture.kind == 3)
            .expect("front cover embedded");
        assert!(image::load_from_memory(&front.data).is_ok());
        let lrc = managed.with_extension("lrc");
        assert_eq!(std::fs::read_to_string(&lrc).expect("lrc written"), synced);
        let cover_file = managed.parent().expect("album folder").join("cover.png");
        assert_eq!(std::fs::read(&cover_file).expect("cover written"), cover);

        let undo = management
            .undo_preview(
                &run.job_id,
                applied.job.row_revision,
                "undo-parity",
                "admin",
            )
            .expect("undo preview");
        assert!(management_step(&library).await, "undo planning runs");
        let undone = management_apply(&library, &undo).await;
        assert_eq!(undone.job.state.as_str(), "succeeded");
        let restored = read_document(&file).expect("tags restored");
        assert_eq!(
            restored.native.get("vorbis:MOOD"),
            Some(&vec!["Calm".to_owned()]),
            "the scrubbed tag is back"
        );
        assert_eq!(restored.values, before.values, "every field as it was");
        assert_eq!(
            restored
                .pictures
                .iter()
                .map(|picture| &picture.data)
                .collect::<Vec<_>>(),
            before
                .pictures
                .iter()
                .map(|picture| &picture.data)
                .collect::<Vec<_>>(),
            "the pictures as they were"
        );
        assert!(!lrc.exists(), "the lyrics file is gone");
        assert!(!cover_file.exists(), "the cover file is gone");
        library
            .settings
            .store()
            .save_blocking(stored)
            .expect("profile back");
        normalize();
    }

    // The editor shows the file; a saved edit writes it and keeps it.
    let editor = management.tag_editor(&track).expect("tag editor");
    assert!(editor.accepted_identity, "{:?}", editor.identity_reason);
    assert!(
        editor
            .fields
            .iter()
            .any(|field| field.field_name == "title")
    );
    let edit = |mode: TagEditMode, value: &str, key: &str| TagEditRequest {
        local_track_id: track.clone(),
        mode,
        expected_settings_revision: revision(),
        expected_policy_revision: policy.clone(),
        fields: vec![("title".to_owned(), serde_json::json!(value))],
        idempotency_key: Some(key.to_owned()),
    };
    let saved = management
        .create_tag_edit(
            edit(TagEditMode::SaveOverride, "Edited Title", "edit-1"),
            "admin",
        )
        .expect("edit preview");
    assert!(management_step(&library).await, "edit planning runs");
    let applied = management_apply(&library, &saved).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    assert_eq!(title(&file), vec!["Edited Title".to_owned()]);
    assert_eq!(overrides(&library), 1, "the edit is kept");

    // Its undo is not blocked by the value the edit itself kept.
    let undo = management
        .undo_preview(
            &saved.job_id,
            applied.job.row_revision,
            "undo-edit",
            "admin",
        )
        .expect("undo preview");
    assert!(management_step(&library).await, "undo planning runs");
    let undone = management_apply(&library, &undo).await;
    assert_eq!(undone.job.state.as_str(), "succeeded");
    assert_eq!(title(&file), original[&TagField::Title]);
    assert_eq!(overrides(&library), 0, "the undo drops the kept value too");

    // Keeping the value the file already has writes nothing but keeps it;
    // a reset that changes no tag only drops it again.
    let current = original[&TagField::Title].join("; ");
    let keep = management
        .create_tag_edit(
            edit(TagEditMode::SaveOverride, &current, "edit-keep"),
            "admin",
        )
        .expect("keep preview");
    assert!(management_step(&library).await, "keep planning runs");
    let kept = management_apply(&library, &keep).await;
    assert_eq!(kept.job.terminal_code.as_deref(), Some("TAG_EDIT_SAVED"));
    assert_eq!(overrides(&library), 1, "the value is kept");
    let reset = management
        .create_tag_edit(edit(TagEditMode::ResetCanonical, "", "edit-reset"), "admin")
        .expect("reset preview");
    assert!(management_step(&library).await, "reset planning runs");
    let reset = management_apply(&library, &reset).await;
    assert_eq!(reset.job.state.as_str(), "succeeded");
    assert_eq!(overrides(&library), 0, "the kept value is gone");

    // Write once, then back to the original.
    let once = management
        .create_tag_edit(edit(TagEditMode::WriteOnce, "Once", "edit-once"), "admin")
        .expect("write-once preview");
    assert!(management_step(&library).await, "write-once planning runs");
    management_apply(&library, &once).await;
    assert_eq!(title(&file), vec!["Once".to_owned()]);
    let restore = management
        .create_restore_preview(
            RestoreRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album.clone()],
                    catalog_filter: None,
                },
                expected_settings_revision: revision(),
                expected_policy_revision: policy.clone(),
                idempotency_key: "restore-1".to_owned(),
            },
            "admin",
        )
        .expect("restore preview");
    assert!(management_step(&library).await, "restore planning runs");
    let restored = management_apply(&library, &restore).await;
    assert_eq!(restored.job.state.as_str(), "succeeded");
    assert_eq!(read_fields(&file).expect("restored"), original);

    // The purge drops the originals only with the typed confirmation.
    let impact = management.purge_impact().expect("purge impact");
    assert_eq!(impact.baseline_count, 1);
    let purge = |phrase: &str, key: &str| PurgeRequest {
        impact_token: impact.impact_token.clone(),
        typed_confirmation: phrase.to_owned(),
        idempotency_key: key.to_owned(),
    };
    assert!(management.purge(purge("yes", "purge-0"), "admin").is_err());
    let purged = management
        .purge(purge("CONFIRM", "purge-1"), "admin")
        .expect("purged");
    assert_eq!(purged.purged_baseline_count, 1);
    assert_eq!(management.purge_impact().expect("impact").baseline_count, 0);
    assert_eq!(
        management
            .recovery_diagnostics()
            .expect("diagnostics")
            .needs_attention_count,
        0
    );

    // A confirmed dry run turns automatic management on for the folder.
    let mut proposed = library
        .settings
        .store()
        .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
        .expect("settings read");
    let assignment = proposed
        .root_assignments
        .iter_mut()
        .find(|assignment| assignment.root_id == "music");
    match assignment {
        Some(assignment) => {
            assignment.enabled = true;
            assignment.automatic_scan_discovered = true;
        }
        None => proposed.root_assignments.push(
            droppedneedle_kernel::runtime_config::sections::LibraryManagementRootAssignment {
                root_id: "music".to_owned(),
                profile_id: Some(lm::PICARD_ORGANIZER_PROFILE_ID.to_owned()),
                enabled: true,
                automatic_scan_discovered: true,
                ..Default::default()
            },
        ),
    }
    let dry_run = management
        .create_activation(
            ActivationRequest {
                root_id: "music".to_owned(),
                settings: proposed.clone(),
                expected_settings_revision: revision(),
                expected_policy_revision: policy.clone(),
                idempotency_key: Some("dry-run".to_owned()),
            },
            "admin",
        )
        .expect("dry run");
    assert!(management_step(&library).await, "dry run planning runs");
    assert!(
        management
            .activation_detail(&dry_run.job_id)
            .expect("dry run detail")
            .ready_for_confirmation
    );
    // A dry run proves settings; it is never applied.
    assert!(management.reissue(&dry_run.job_id, "admin").is_err());
    let refused = management.apply(
        &dry_run.job_id,
        droppedneedle_library::management::service::ApplyRequest {
            preview_token: dry_run.preview_token.clone(),
            expected_row_revision: management
                .detail(&dry_run.job_id)
                .expect("detail")
                .job
                .row_revision,
            idempotency_key: "apply-dry-run".to_owned(),
            confirmation: true,
        },
    );
    assert!(refused.is_err(), "an activation preview cannot be applied");
    management
        .confirm_activation(
            ActivationConfirm {
                settings: proposed,
                proofs: vec![ActivationProof {
                    root_id: "music".to_owned(),
                    job_id: dry_run.job_id.clone(),
                    preview_token: dry_run.preview_token.clone(),
                }],
                expected_settings_revision: revision(),
                confirmation: true,
            },
            "admin",
        )
        .expect("activation confirmed");
    let proved = management.detail(&dry_run.job_id).expect("detail").job;
    assert_eq!(
        (proved.state.as_str(), proved.terminal_code.as_deref()),
        ("succeeded", Some("ACTIVATION_CONFIRMED")),
        "a confirmed dry run is done"
    );

    // The album is then managed without asking: moved by the profile.
    library.identification.queue_automatic_management(&album);
    assert!(automatic_next(&library).await);
    assert!(management_step(&library).await, "automatic planning runs");
    assert!(management_step(&library).await, "automatic apply runs");
    assert!(!file.exists(), "the profile moved the file");
    let (history, _) = management.history(Default::default()).expect("history");
    let automatic = history
        .iter()
        .find(|item| item.origin == "scan_discovered")
        .expect("an automatic run");
    assert_eq!(automatic.mode, "automatic_apply");
    assert_eq!(automatic.job.state.as_str(), "succeeded");

    // Looked at again, the album is already managed: nothing new runs.
    library.identification.queue_automatic_management(&album);
    assert!(automatic_next(&library).await);
    let (again, _) = management.history(Default::default()).expect("history");
    assert_eq!(again.len(), history.len());

    // A new match revision (a new edition) is managed again.
    let db = || {
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db")
    };
    db().execute(
        "UPDATE local_album_external_identities SET row_revision = row_revision + 1 \
         WHERE local_album_id = ?1",
        [&album],
    )
    .expect("identity bumped");
    library.identification.queue_automatic_management(&album);
    assert!(automatic_next(&library).await);
    let (renewed, _) = management.history(Default::default()).expect("history");
    assert_eq!(
        renewed.len(),
        history.len() + 1,
        "a new edition is managed again"
    );
    assert!(management_step(&library).await, "re-planning runs");
    management_step(&library).await;

    // A second copy lands where the managed file sits: the clash is
    // settled by replacing the file in the way through the publisher: it
    // is set aside under the journal and recycled after the commit.
    let managed_rel: String = db()
        .query_row(
            "SELECT relative_path FROM local_tracks WHERE id = ?1",
            [&track],
            |row| row.get(0),
        )
        .expect("managed track path");
    let second = plant(&music, "second/tool track.flac", "management_full.flac");
    rescan(&library).await;
    let second_track = library
        .scan_store
        .track_at("music", "second/tool track.flac")
        .unwrap()
        .expect("second copy indexed");
    let second_album = library
        .scan_store
        .album_for_track(&second_track)
        .unwrap()
        .expect("second album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &second_album));
    library.identification.identify_tick().await;
    crate::common::library::operations(&library)
        .confirm_match(&second_album, "admin")
        .expect("second match confirmed");
    let settings_now = revision();
    let clash = management
        .create_preview(
            droppedneedle_library::management::service::PreviewRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![second_album.clone()],
                    catalog_filter: None,
                },
                profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                expected_settings_revision: settings_now.clone(),
                expected_policy_revision: policy.clone(),
                idempotency_key: Some("clash".to_owned()),
                target_root_id: None,
                overrides: None,
            },
            "admin",
        )
        .expect("clash preview");
    assert!(management_step(&library).await, "clash planning runs");
    let blocked = management
        .items(&clash.job_id, -1, 10, &Default::default())
        .expect("clash items")
        .items
        .into_iter()
        .find(|item| {
            item.eligibility == droppedneedle_library::management::models::Eligibility::Blocked
        })
        .expect("the copy is blocked by the clash");
    let source = management.detail(&clash.job_id).expect("clash detail");
    let replace_request = |key: &str| {
        droppedneedle_library::management::duplicates::DuplicateRequest {
        intent: droppedneedle_library::management::models::DuplicateIntent {
            source_job_id: clash.job_id.clone(),
            source_plan_item_ordinal: blocked.ordinal,
            collision_kind: "same_path_different_content".to_owned(),
            existing_root_id: "music".to_owned(),
            existing_relative_path: managed_rel.clone(),
            existing_local_track_id: None,
            action: droppedneedle_library::management::models::DuplicateAction::RecycleExistingKeepIncoming,
            alternate_relative_path: None,
        },
        expected_source_row_revision: source.job.row_revision,
        expected_settings_revision: settings_now.clone(),
        expected_policy_revision: policy.clone(),
        idempotency_key: key.to_owned(),
    }
    };
    let apply_request = |handle: &droppedneedle_library::management::models::PreviewHandle| {
        droppedneedle_library::management::service::ApplyRequest {
            preview_token: handle.preview_token.clone(),
            expected_row_revision: management
                .detail(&handle.job_id)
                .expect("detail")
                .job
                .row_revision,
            idempotency_key: format!("apply-{}", handle.job_id),
            confirmation: true,
        }
    };
    let stop = |job_id: &str| {
        crate::common::library::operations(&library)
            .control(
                job_id,
                droppedneedle_library::operations::models::Control::Stop,
                management.detail(job_id).expect("detail").job.row_revision,
                None,
            )
            .expect("stopped while queued")
    };
    let resolve = management
        .create_duplicate_preview(replace_request("resolve-clash"), "admin")
        .expect("duplicate preview");
    assert!(management_step(&library).await, "duplicate planning runs");
    let before_apply = std::fs::read(music.join(&managed_rel)).expect("existing bytes");
    let planned = management
        .items(&resolve.job_id, -1, 10, &Default::default())
        .expect("resolve items")
        .items;
    assert!(
        planned.iter().any(|item| item
            .facts
            .warnings
            .iter()
            .any(|warning| warning.contains(".recycle"))),
        "the preview names the recycle bin"
    );

    // A stop before the worker starts touches nothing.
    management
        .apply(&resolve.job_id, apply_request(&resolve))
        .expect("resolution queued");
    assert_eq!(stop(&resolve.job_id).state.as_str(), "stopped");
    management_step(&library).await;
    library.publishing.publish_tick().expect("maintenance runs");
    assert_eq!(
        std::fs::read(music.join(&managed_rel)).expect("bytes in place"),
        before_apply,
        "a stopped resolution leaves the file in the way where it was"
    );
    assert!(second.exists(), "the new copy stays where it was");
    assert!(walk_files(&music.join(".recycle")).is_empty());

    // The process dies right after the file in the way was set aside and
    // the new copy renamed into place, before the catalog commit. The
    // publish journal holds both files; the album waits for recovery.
    let resolve = management
        .create_duplicate_preview(replace_request("resolve-clash-again"), "admin")
        .expect("second duplicate preview");
    assert!(
        management_step(&library).await,
        "duplicate planning runs again"
    );
    library.publishing.arm_publish_crash(Some(
        droppedneedle_library::publish::CrashPoint::AfterFirstRename,
    ));
    management
        .apply(&resolve.job_id, apply_request(&resolve))
        .expect("resolution queued");
    management_step(&library).await;
    library.publishing.arm_publish_crash(None);
    assert_eq!(
        management
            .detail(&resolve.job_id)
            .expect("detail")
            .job
            .state
            .as_str(),
        "queued",
        "the album waits for recovery instead of failing"
    );
    assert!(
        walk_files(&music.join(".recycle")).is_empty(),
        "nothing reaches the recycle bin before the commit"
    );
    assert_eq!(
        walk_files(&music)
            .iter()
            .filter(|path| std::fs::read(path).ok().as_deref() == Some(before_apply.as_slice()))
            .count(),
        1,
        "the file in the way is kept, set aside"
    );
    // A stop while it waits does not strand it: the journal owns the
    // files, and recovery settles the publish either way.
    assert_eq!(stop(&resolve.job_id).state.as_str(), "stopped");
    library.publishing.publish_tick().expect("recovery runs");
    let holder: (String, String) = db()
        .query_row(
            "SELECT id, availability FROM local_tracks WHERE root_id = 'music' \
             AND relative_path = ?1",
            [&managed_rel],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the path is listed");
    assert_eq!(
        holder,
        (second_track.clone(), "indexed".to_owned()),
        "recovery finished the publish: the new copy holds the path"
    );
    assert!(!second.exists(), "the new copy moved into place");
    let recycled = walk_files(&music.join(".recycle"));
    let in_bin: Vec<&PathBuf> = recycled
        .iter()
        .filter(|path| std::fs::read(path).ok().as_deref() == Some(before_apply.as_slice()))
        .collect();
    assert_eq!(
        in_bin.len(),
        1,
        "the file in the way is in the recycle bin, once"
    );
    let named: String = db()
        .query_row(
            "SELECT recycle_to FROM library_publish_replacements WHERE recycled_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("the bin path is recorded");
    assert_eq!(
        PathBuf::from(&named),
        *in_bin[0],
        "the journal names the bin path"
    );
    let retired: (String, String) = db()
        .query_row(
            "SELECT availability, file_path FROM local_tracks WHERE id = ?1",
            [&track],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("replaced track kept");
    assert_eq!(retired, ("missing".to_owned(), named.clone()));
    assert!(
        management
            .detail(&resolve.job_id)
            .expect("detail")
            .undo_available_count
            > 0,
        "an album recovery finished after the stop can still be undone"
    );
}

/// A custom edition (files sealed under a release group, no release) is
/// managed from its manifest, writes no release ids, and waits once its
/// files no longer match what was sealed.
#[tokio::test]
async fn library_management_custom_edition_journey() {
    use droppedneedle_library::management::models::{Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::tags::{TagField, read_fields};

    let (scratch, library, music) = bare_library("lib-management-custom").await;
    let file = plant(&music, "custom/sealed track.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "custom/sealed track.flac")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    crate::common::library::operations(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    library
        .settings
        .store()
        .save_blocking(lm::initial_settings(""))
        .expect("settings saved");
    lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
        .get_settings()
        .expect("settings normalized");
    // Seal the album as a custom edition: its release is let go, and the
    // manifest records the files as they are.
    let db = || {
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db")
    };
    let release_id_before =
        read_fields(&file).expect("tags")[&TagField::MusicBrainzReleaseId].clone();
    db().execute_batch(&format!(
        "UPDATE local_album_external_identities SET release_mbid = NULL, \
         row_revision = row_revision + 1 WHERE local_album_id = '{album}'; \
         UPDATE local_track_external_identities SET release_mbid = NULL, \
         release_track_mbid = NULL WHERE local_track_id = '{track}'; \
         INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
         release_group_mbid, album_title, album_artist_name, artist_mbid, album_metadata_json, \
         source_album_revision, input_revision, content_hash, sealed_by_user_id, sealed_at) \
         VALUES ('manifest-1', '{album}', 1, 'group-1', 'Sealed Album', 'Sealed Artist', NULL, \
         '{{\"year\": 2001}}', 1, 'input', 'hash', 'admin', 0); \
         INSERT INTO library_custom_edition_tracks (manifest_id, ordinal, local_track_id, \
         source_track_revision, stat_revision, tag_revision, title, artist_name, album_title, \
         album_artist_name, disc_number, track_number) VALUES ('manifest-1', 0, '{track}', 1, \
         's', 't', 'Sealed Title', 'Sealed Artist', 'Sealed Album', 'Sealed Artist', 1, 1); \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, activated_at) \
         VALUES ('{album}', 'manifest-1', 0);"
    ))
    .expect("custom edition sealed");
    let management = Management::new(&library);
    let revision = lm::settings_revision(
        &library
            .settings
            .store()
            .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
            .expect("settings read"),
    );
    let preview = |key: &str| PreviewRequest {
        selection: Selection {
            kind: SelectionKind::Albums,
            ids: vec![album.clone()],
            catalog_filter: None,
        },
        profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
        expected_settings_revision: revision.clone(),
        expected_policy_revision: library.core.policy_revision().expect("policy revision"),
        idempotency_key: Some(key.to_owned()),
        target_root_id: None,
        overrides: None,
    };
    let handle = management
        .create_preview(preview("custom-1"), "admin")
        .expect("custom preview");
    assert!(management_step(&library).await, "custom planning runs");
    let applied = management_apply(&library, &handle).await;
    assert_eq!(
        applied.job.state.as_str(),
        "succeeded",
        "{:?}",
        applied.job.terminal_code
    );
    let rel: String = db()
        .query_row(
            "SELECT relative_path FROM local_tracks WHERE id = ?1",
            [&track],
            |row| row.get(0),
        )
        .expect("managed path");
    let fields = read_fields(&music.join(&rel)).expect("managed tags");
    assert_eq!(fields[&TagField::Title], vec!["Sealed Title".to_owned()]);
    assert_eq!(fields[&TagField::Album], vec!["Sealed Album".to_owned()]);
    assert_eq!(
        fields[&TagField::MusicBrainzReleaseId],
        release_id_before,
        "no release id is written for a custom edition"
    );

    // A file added since the seal: the album waits for a new seal.
    plant(&music, "custom/added track.flac", "management_full.flac");
    rescan(&library).await;
    let extra = library
        .scan_store
        .track_at("music", "custom/added track.flac")
        .unwrap()
        .expect("added track indexed");
    db().execute(
        "UPDATE local_tracks SET local_album_id = ?1 WHERE id = ?2",
        [&album, &extra],
    )
    .expect("added to the album");
    let handle = management
        .create_preview(preview("custom-2"), "admin")
        .expect("second preview");
    assert!(management_step(&library).await, "second planning runs");
    let items = management
        .items(&handle.job_id, -1, 10, &Default::default())
        .expect("items")
        .items;
    assert!(
        items.iter().all(|item| item
            .reason()
            .is_some_and(|reason| reason.code == "RELEASE_NOT_SELECTED")),
        "a stale manifest blocks the album"
    );
}

/// Embedded art on an M4A file with a scrub on: the cover lands once and
/// a second run leaves the pictures alone (MP4 stores no picture types),
/// and the scrub keeps composer, work ids and ISRC while it removes an
/// unmanaged tag.
#[tokio::test]
async fn library_management_artwork_journey() {
    use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
    use droppedneedle_library::management::enrich::{Cover, ScriptedEnrichment};
    use droppedneedle_library::management::models::{Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::tags::save::read_document;
    use droppedneedle_library::tags::{TagEdit, TagField};

    let cover = {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(8, 8, image::Rgb([30, 30, 200]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        bytes
    };
    let (scratch, library, music) = bare_library_with(
        "lib-management-artwork",
        Some(Arc::new(ScriptedEnrichment {
            listenbrainz: None,
            lyrics: None,
            cover: Some(Cover {
                bytes: cover,
                mime: "image/png".to_owned(),
            }),
            ..Default::default()
        })),
    )
    .await;
    let file = plant(&music, "art/cover track.m4a", "management_full.m4a");
    droppedneedle_library::tags::save_tags(
        &file,
        &[
            TagEdit::native("mp4:\u{a9}wrt", vec!["Ada Composer".to_owned()]),
            TagEdit::native(
                "mp4:----:MusicBrainz Work Id",
                vec!["c0ffee00-0000-4000-8000-00000000f001".to_owned()],
            ),
            TagEdit::native("mp4:----:MOOD", vec!["Calm".to_owned()]),
            TagEdit::new(TagField::Isrc, vec!["GBAYE0601498".to_owned()]),
        ],
    )
    .expect("tags planted");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "art/cover track.m4a")
        .unwrap()
        .expect("track indexed");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scratch
        .identify
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identification.identify_tick().await, 1);
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    crate::common::library::operations(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    let mut settings = lm::initial_settings("");
    for profile in settings
        .profiles
        .iter_mut()
        .filter(|profile| profile.id == lm::PICARD_ORGANIZER_PROFILE_ID)
    {
        profile.artwork.embedded_enabled = true;
        profile.artwork.external_enabled = true;
        profile.metadata.scrub_unmanaged_tags = true;
    }
    library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");
    let original = read_document(&file).expect("original tags");
    lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
        .get_settings()
        .expect("settings normalized");
    let db = || {
        droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
            .expect("db")
    };
    let management = Management::new(&library);
    let preview = |key: &str| PreviewRequest {
        selection: Selection {
            kind: SelectionKind::Albums,
            ids: vec![album.clone()],
            catalog_filter: None,
        },
        profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
        expected_settings_revision: lm::settings_revision(
            &library
                .settings
                .store()
                .get::<LibraryManagement>()
                .expect("settings read"),
        ),
        expected_policy_revision: library.core.policy_revision().expect("policy revision"),
        idempotency_key: Some(key.to_owned()),
        target_root_id: None,
        overrides: None,
    };
    let managed_path = || {
        let rel: String = db()
            .query_row(
                "SELECT relative_path FROM local_tracks WHERE id = ?1",
                [&track],
                |row| row.get(0),
            )
            .expect("managed path");
        music.join(rel)
    };
    let managed_doc = || read_document(&managed_path()).expect("managed tags");

    let first = management
        .create_preview(preview("art-1"), "admin")
        .expect("first preview");
    assert!(management_step(&library).await, "first planning runs");
    let items = management
        .items(&first.job_id, -1, 10, &Default::default())
        .expect("items")
        .items;
    let item = items.first().expect("one file");
    assert!(item.eligibility.writes(), "{:?}", item.facts);
    assert!(item.diff.artwork_changed, "the cover is embedded");
    let scrubbed: Vec<&str> = item
        .diff
        .scrubbed_raw_tags
        .iter()
        .map(|tag| tag.key.as_str())
        .collect();
    assert!(scrubbed.contains(&"mp4:----:MOOD"), "{scrubbed:?}");
    assert!(
        scrubbed
            .iter()
            .all(|key| !key.contains("\u{a9}wrt") && !key.contains("MusicBrainz")),
        "composer and MusicBrainz ids are never scrubbed: {scrubbed:?}"
    );
    let applied = management_apply(&library, &first).await;
    assert_eq!(
        applied.job.state.as_str(),
        "succeeded",
        "{:?}",
        applied.job.terminal_code
    );
    let after = managed_doc();
    assert_eq!(after.pictures.len(), 1, "one cover, nothing piled up");
    assert_eq!(after.pictures[0].kind, 3);
    assert!(!after.native.contains_key("mp4:----:MOOD"), "scrubbed");
    // Composer and work id are fields now: read back from the file's
    // own atoms, not listed as native items.
    assert_eq!(
        after.values.get(&TagField::Composer),
        Some(&vec!["Ada Composer".to_owned()]),
        "composer kept"
    );
    assert_eq!(
        after.values.get(&TagField::MusicBrainzWorkId),
        Some(&vec!["c0ffee00-0000-4000-8000-00000000f001".to_owned()]),
        "work id kept"
    );
    assert!(
        !after.native.contains_key("mp4:\u{a9}wrt"),
        "composer is a field, not a native item"
    );
    assert_eq!(
        after.values.get(&TagField::Isrc),
        Some(&vec!["GBAYE0601498".to_owned()]),
        "ISRC kept"
    );

    // Run it again: the cover is already there, so the pictures stay as
    // they are (no remove and re-embed, no second copy).
    let second = management
        .create_preview(preview("art-2"), "admin")
        .expect("second preview");
    assert!(management_step(&library).await, "second planning runs");
    let items = management
        .items(&second.job_id, -1, 10, &Default::default())
        .expect("items")
        .items;
    let item = items.first().expect("one file");
    assert!(
        !item.diff.artwork_changed,
        "{:?}",
        item.diff.field_mutations
    );
    assert!(
        item.diff
            .field_mutations
            .iter()
            .all(|mutation| mutation.name != "pictures"),
        "{:?}",
        item.diff.field_mutations
    );
    assert!(item.diff.scrubbed_raw_tags.is_empty());
    assert!(item.diff.generated.is_empty(), "the cover file is there");

    // Restoring the originals takes the generated cover file away too.
    let cover_file = managed_path()
        .parent()
        .expect("album folder")
        .join("cover.png");
    assert!(cover_file.exists(), "the cover file was written");
    let restore = management
        .create_restore_preview(
            droppedneedle_library::management::restore::RestoreRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album.clone()],
                    catalog_filter: None,
                },
                expected_settings_revision: lm::settings_revision(
                    &library
                        .settings
                        .store()
                        .get::<LibraryManagement>()
                        .expect("settings read"),
                ),
                expected_policy_revision: library.core.policy_revision().expect("policy revision"),
                idempotency_key: "art-restore".to_owned(),
            },
            "admin",
        )
        .expect("restore preview");
    assert!(management_step(&library).await, "restore planning runs");
    let restored = management_apply(&library, &restore).await;
    assert_eq!(
        restored.job.state.as_str(),
        "succeeded",
        "{:?}",
        restored.job.terminal_code
    );
    assert!(!cover_file.exists(), "the generated cover is gone");
    let back = read_document(&file).expect("restored tags");
    assert_eq!(back.native, original.native, "the scrubbed tag is back");
    assert_eq!(
        back.pictures
            .iter()
            .map(|picture| &picture.data)
            .collect::<Vec<_>>(),
        original
            .pictures
            .iter()
            .map(|picture| &picture.data)
            .collect::<Vec<_>>(),
        "the pictures as they were"
    );
}

/// A library whose `music` folder is managed by the Picard-style profile
/// without asking, once [`ManagedLibrary::activate`] has confirmed a dry
/// run for it.
struct ManagedLibrary {
    scratch: Scratch,
    library: LibrarySetup,
    music: PathBuf,
    /// Albums added so far.
    albums: Cell<u32>,
}

impl ManagedLibrary {
    /// An empty library with its settings saved; `configure` changes the
    /// profile before anything pins it.
    async fn start(
        tag: &str,
        configure: impl Fn(
            &mut droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
        ),
    ) -> Self {
        Self::start_with(tag, configure, None).await
    }

    /// As [`Self::start`], planning through the scripted outside sources.
    async fn scripted(
        tag: &str,
        configure: impl Fn(
            &mut droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
        ),
        enrichment: Arc<droppedneedle_library::management::enrich::ScriptedEnrichment>,
    ) -> Self {
        Self::start_with(tag, configure, Some(enrichment)).await
    }

    async fn start_with(
        tag: &str,
        configure: impl Fn(
            &mut droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
        ),
        enrichment: Option<Arc<dyn droppedneedle_library::management::enrich::EnrichmentPort>>,
    ) -> Self {
        use droppedneedle_library::management::settings as lm;
        let (scratch, library, music) = bare_library_with(tag, enrichment).await;
        std::fs::create_dir_all(&music).expect("music folder");
        add_music_root(&library, &music).await;
        library
            .scan_store
            .db()
            .run_sql(
                "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
            )
            .expect("admin row");
        let mut settings = lm::initial_settings("");
        for profile in settings
            .profiles
            .iter_mut()
            .filter(|profile| profile.id == lm::PICARD_ORGANIZER_PROFILE_ID)
        {
            configure(profile);
        }
        library
            .settings
            .store()
            .save_blocking(settings)
            .expect("settings saved");
        lm::service::LibraryManagementService::new(Arc::new(library.settings.store().clone()))
            .get_settings()
            .expect("settings normalized");
        Self {
            scratch,
            library,
            music,
            albums: Cell::new(0),
        }
    }

    fn db(&self) -> rusqlite::Connection {
        droppedneedle_kernel::db::open_connection(&self.scratch.to_path_buf().join("app.db"))
            .expect("db opens")
    }

    fn management(&self) -> droppedneedle_library::management::service::Management {
        droppedneedle_library::management::service::Management::new(&self.library)
    }

    fn settings_revision(&self) -> String {
        droppedneedle_library::management::settings::settings_revision(
            &self
                .library
                .settings
                .store()
                .get::<droppedneedle_kernel::runtime_config::sections::LibraryManagement>()
                .expect("settings read"),
        )
    }

    /// Plant `tracks` copies of the fixture as one album in `folder`,
    /// scan, identify it and confirm the match. Returns the album and its
    /// tracks in order.
    async fn add_album(&self, folder: &str, tracks: usize) -> (String, Vec<String>) {
        self.add_album_from(folder, &vec!["management_full.flac"; tracks], |_| {})
            .await
    }

    /// [`Self::add_album`] from these fixtures, one track each, with the
    /// editions changed by `edition` before the match is confirmed.
    async fn add_album_from(
        &self,
        folder: &str,
        fixtures: &[&str],
        edition: impl Fn(&mut Release),
    ) -> (String, Vec<String>) {
        use droppedneedle_library::tags::TagField;
        use droppedneedle_library::tags::save::{TagEdit, save_tags};
        let nth = self.albums.replace(self.albums.get() + 1);
        let mut relative = Vec::new();
        for (index, fixture) in fixtures.iter().enumerate() {
            let number = index + 1;
            let extension = Path::new(fixture)
                .extension()
                .and_then(|extension| extension.to_str())
                .expect("fixture extension");
            let rel = format!("{folder}/{number:02}.{extension}");
            let file = plant(&self.music, &rel, fixture);
            // Its own title, so no two files are named alike.
            let mut edits = vec![TagEdit::verbatim(
                TagField::Title,
                vec![format!("{folder} {number}")],
            )];
            // Every album after the first is an album of its own, not
            // another copy of the fixture's.
            if nth > 0 {
                edits.extend([
                    TagEdit::verbatim(TagField::Album, vec![folder.to_owned()]),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseId, Vec::new()),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseGroupId, Vec::new()),
                ]);
            }
            if number > 1 {
                edits.extend([
                    TagEdit::verbatim(TagField::TrackNumber, vec![number.to_string()]),
                    TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
                ]);
            }
            save_tags(&file, &edits).expect("track retagged");
            relative.push(rel);
        }
        self.index_album(nth, &relative, edition).await
    }

    /// The fixture planted byte for byte, never retagged, as the only file
    /// of the first album, so it keeps the fixture's release and track ids.
    async fn add_untouched_album(&self, folder: &str, fixture: &str) -> (String, Vec<String>) {
        let nth = self.albums.replace(self.albums.get() + 1);
        assert_eq!(nth, 0, "only the first album keeps the fixture's ids");
        let extension = Path::new(fixture)
            .extension()
            .and_then(|extension| extension.to_str())
            .expect("fixture extension");
        let rel = format!("{folder}/01.{extension}");
        plant(&self.music, &rel, fixture);
        self.index_album(nth, &[rel], |_| {}).await
    }

    /// Scan the `nth` album's planted files, identify the album and
    /// confirm the match.
    async fn index_album(
        &self,
        nth: u32,
        relative: &[String],
        edition: impl Fn(&mut Release),
    ) -> (String, Vec<String>) {
        rescan(&self.library).await;
        let ids: Vec<String> = relative
            .iter()
            .map(|rel| {
                self.library
                    .scan_store
                    .track_at("music", rel)
                    .unwrap()
                    .expect("track indexed")
            })
            .collect();
        let album = self
            .library
            .scan_store
            .album_for_track(&ids[0])
            .unwrap()
            .expect("album");
        // The first album gets `REL_1` and `REL_2`. Every later one gets
        // editions of its own, or all would be named from one release.
        let mut recall = ambiguous_recall(&self.library.identify_store, &album);
        if nth > 0 {
            for (edition, release) in recall.releases.iter_mut().enumerate() {
                release.id = format!("{nth:08x}-{edition:04x}-4111-8111-111111111111");
                release.release_group_id = format!("rg-{nth}-{edition}");
            }
        }
        recall.releases.iter_mut().for_each(&edition);
        self.scratch.identify.set_recall(recall);
        assert_eq!(self.library.identification.identify_tick().await, 1);
        crate::common::library::operations(&self.library)
            .confirm_match(&album, "admin")
            .expect("match confirmed");
        (album, ids)
    }

    /// Turn automatic management on for the folder: a dry run, confirmed.
    async fn activate(&self) {
        use droppedneedle_library::management::activation::{ActivationConfirm, ActivationProof};
        let (dry_run, proposed) = self.dry_run("dry-run", |_| {}).await;
        self.management()
            .confirm_activation(
                ActivationConfirm {
                    settings: proposed,
                    proofs: vec![ActivationProof {
                        root_id: "music".to_owned(),
                        job_id: dry_run.job_id,
                        preview_token: dry_run.preview_token,
                    }],
                    expected_settings_revision: self.settings_revision(),
                    confirmation: true,
                },
                "admin",
            )
            .expect("activation confirmed");
    }

    /// A planned dry run for turning automatic management on for the
    /// folder, with the settings `change` made, and those settings.
    async fn dry_run(
        &self,
        key: &str,
        change: impl Fn(&mut droppedneedle_kernel::runtime_config::sections::LibraryManagement),
    ) -> (
        droppedneedle_library::management::models::PreviewHandle,
        droppedneedle_kernel::runtime_config::sections::LibraryManagement,
    ) {
        use droppedneedle_kernel::runtime_config::sections::{
            LibraryManagement, LibraryManagementRootAssignment,
        };
        use droppedneedle_library::management::activation::ActivationRequest;
        let management = self.management();
        // Identification queued the albums while nothing was automatic
        // yet; the worker looks and drops them.
        while automatic_next(&self.library).await {}
        let mut proposed = self
            .library
            .settings
            .store()
            .get::<LibraryManagement>()
            .expect("settings read");
        match proposed
            .root_assignments
            .iter_mut()
            .find(|assignment| assignment.root_id == "music")
        {
            Some(assignment) => {
                assignment.enabled = true;
                assignment.automatic_scan_discovered = true;
            }
            None => proposed
                .root_assignments
                .push(LibraryManagementRootAssignment {
                    root_id: "music".to_owned(),
                    profile_id: Some(
                        droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID
                            .to_owned(),
                    ),
                    enabled: true,
                    automatic_scan_discovered: true,
                    ..Default::default()
                }),
        }
        change(&mut proposed);
        let dry_run = management
            .create_activation(
                ActivationRequest {
                    root_id: "music".to_owned(),
                    settings: proposed.clone(),
                    expected_settings_revision: self.settings_revision(),
                    expected_policy_revision: self
                        .library
                        .core
                        .policy_revision()
                        .expect("policy revision"),
                    idempotency_key: Some(key.to_owned()),
                },
                "admin",
            )
            .expect("dry run");
        assert!(
            management_step(&self.library).await,
            "dry run planning runs"
        );
        (dry_run, proposed)
    }

    /// Have automatic management look at the album now, and run whatever
    /// it starts to the end.
    async fn look(&self, album: &str) {
        self.library
            .identification
            .queue_automatic_management(album);
        self.worker_looks().await;
    }

    /// Let the worker look at every waiting album and finish its runs.
    async fn worker_looks(&self) {
        while automatic_next(&self.library).await {}
        while management_step(&self.library).await {}
    }

    /// Where the track's file is now.
    fn path(&self, track: &str) -> PathBuf {
        let rel: String = self
            .db()
            .query_row(
                "SELECT relative_path FROM local_tracks WHERE id = ?1",
                [track],
                |row| row.get(0),
            )
            .expect("track path");
        self.music.join(rel)
    }

    /// The bytes the app recorded for the track's file.
    fn recorded(&self, track: &str) -> Option<String> {
        self.db()
            .query_row(
                "SELECT written_sha256 FROM library_track_management_state \
                 WHERE local_track_id = ?1",
                [track],
                |row| row.get(0),
            )
            .unwrap_or(None)
    }

    /// The automatic runs so far, oldest first: state and terminal code.
    fn automatic_runs(&self) -> Vec<(String, Option<String>)> {
        let (history, _) = self
            .management()
            .history(Default::default())
            .expect("history");
        history
            .iter()
            .rev()
            .filter(|item| item.origin == "scan_discovered")
            .map(|item| {
                (
                    item.job.state.as_str().to_owned(),
                    item.job.terminal_code.clone(),
                )
            })
            .collect()
    }
}

fn file_sha256(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path).expect("file reads"));
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The values v2's own code gives for `managed_path_revision`, from a
/// scratch copy of its `_hash_file` and both formula sites (`main` at
/// cf7278a1) run on these fixtures. v3 must compute the same bytes, or
/// every album v2 managed would read as changed after an upgrade.
#[test]
fn v2_path_revision_matches_v2() {
    use droppedneedle_library::publish::record::v2_path_revision;
    let cases = [
        (
            "music",
            "Artist/Album/01 Track.flac",
            "management_full.flac",
            "658fe3ecf890e4dd1d30a9cf56df3c4cabb16b6afb8bd919b234b24cf450dc18",
        ),
        (
            "3f0c9a52-7d1e-4b0a-9c55-0d5a7e1b2c3d",
            "incoming/a.mp3",
            "mp3_full_01.mp3",
            "9f472d49af0a94bd220804a21fc007e8c7a10a966e1c592b70c8ecdf578f8cd7",
        ),
        (
            "music",
            "x.m4a",
            "m4a_full_01.m4a",
            "140c24909442ce3398474f4b775c28b52444c798cd128e76a0b272b82215700e",
        ),
        (
            "music",
            "Bj\u{f6}rk \u{2013} \u{dc}n\u{ef}code \u{2713}/\u{65e5}\u{672c}\u{8a9e}/01 \u{2013} tr\u{101}ck.flac",
            "management_full.flac",
            "10c3af7a12bde5ad2cc771b4c6f489cc60d772b1ead4ed43e487f59905ae8755",
        ),
    ];
    for (root, relative, fixture, expected) in cases {
        let file = file_sha256(&fixtures_dir().join(fixture));
        assert_eq!(
            v2_path_revision(root, relative, &file),
            expected,
            "{relative}"
        );
    }
}

impl ManagedLibrary {
    /// Why the app stepped back from the album, if it did.
    fn mark(&self, album: &str) -> Option<String> {
        use rusqlite::OptionalExtension as _;
        self.db()
            .query_row(
                "SELECT cause FROM library_album_takeovers WHERE local_album_id = ?1",
                [album],
                |row| row.get(0),
            )
            .optional()
            .expect("mark reads")
    }

    fn count(&self, sql: &str) -> i64 {
        self.db()
            .query_row(sql, [], |row| row.get(0))
            .expect("count reads")
    }

    /// Change the track's file from outside the app, then scan.
    async fn edit_outside(&self, track: &str, edit: droppedneedle_library::tags::TagEdit) {
        droppedneedle_library::tags::save_tags(&self.path(track), &[edit])
            .expect("retagged outside the app");
        rescan(&self.library).await;
    }

    /// A person's preview with the folder's profile, planned.
    async fn preview(
        &self,
        kind: droppedneedle_library::management::models::SelectionKind,
        ids: &[&str],
        key: &str,
    ) -> (
        droppedneedle_library::management::models::PreviewHandle,
        Vec<droppedneedle_library::management::models::PlannedItem>,
    ) {
        use droppedneedle_library::management::models::Selection;
        use droppedneedle_library::management::service::PreviewRequest;
        let management = self.management();
        let handle = management
            .create_preview(
                PreviewRequest {
                    selection: Selection {
                        kind,
                        ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                        catalog_filter: None,
                    },
                    profile_id:
                        droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID
                            .to_owned(),
                    expected_settings_revision: self.settings_revision(),
                    expected_policy_revision: self
                        .library
                        .core
                        .policy_revision()
                        .expect("policy revision"),
                    idempotency_key: Some(key.to_owned()),
                    target_root_id: None,
                    overrides: None,
                },
                "admin",
            )
            .expect("preview queued");
        assert!(management_step(&self.library).await, "planning runs");
        let items = management
            .items(&handle.job_id, -1, 100, &Default::default())
            .expect("items")
            .items;
        (handle, items)
    }
}

/// Automatic management settles album by album: catalog changes made for
/// other albums never hold one back. An album is looked at 30 s after it
/// was queued, while no identification job of its own is live and no scan
/// runs. Time is passed in, so nothing sleeps.
#[tokio::test]
async fn automatic_management_settles_per_album() {
    let managed = ManagedLibrary::start("lib-settle", |_| {}).await;
    let (ready, _) = managed.add_album("ready", 1).await;
    let (identifying, _) = managed.add_album("identifying", 1).await;
    let (late, _) = managed.add_album("late", 1).await;
    managed.activate().await;
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs_f64();
    managed
        .library
        .identification
        .queue_automatic_management(&ready);
    managed
        .library
        .identification
        .queue_automatic_management(&identifying);
    managed
        .db()
        .execute(
            "INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
             input_revision, idempotency_key, created_ms, updated_ms) \
             VALUES ('live-job', ?1, 'automatic', 0, 'queued', 'r', 'live-job', 0, 0)",
            [&identifying],
        )
        .expect("live identification job");
    let mut settle = Settle::default();
    let queued = |album: &str| -> bool {
        managed
            .db()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM library_management_automatic_queue \
                 WHERE local_album_id = ?1 AND attempts = 0 AND last_reason_code IS NULL)",
                [album],
                |row| row.get(0),
            )
            .expect("queue read")
    };

    assert!(
        !step_at(&managed, &mut settle, start, 1.0).await,
        "nothing is looked at right after a start"
    );
    assert!(
        step_at(&managed, &mut settle, start, SETTLE_SECONDS + 5.0).await,
        "a due album is looked at while the catalog keeps moving"
    );
    while management_step(&managed.library).await {}
    assert!(!queued(&ready), "the due album was managed");
    assert_eq!(managed.automatic_runs().len(), 1);

    assert!(
        !step_at(&managed, &mut settle, start, SETTLE_SECONDS + 10.0).await,
        "an album with a live identification job waits"
    );
    assert!(queued(&identifying), "waiting spends no try");
    droppedneedle_library::management::automatic::queue(
        &managed.db(),
        &late,
        start + SETTLE_SECONDS + 11.0,
    )
    .expect("queued");
    assert!(
        !step_at(&managed, &mut settle, start, 2.0 * SETTLE_SECONDS + 10.0).await,
        "an album queued later waits its own window"
    );
    assert!(queued(&late));
    assert!(step_at(&managed, &mut settle, start, 2.0 * SETTLE_SECONDS + 12.0).await);
    while management_step(&managed.library).await {}
    assert!(
        !queued(&late),
        "the album was managed once its window passed"
    );
    assert!(queued(&identifying));

    managed
        .db()
        .execute(
            "UPDATE library_identify_jobs SET state = 'succeeded' WHERE id = 'live-job'",
            [],
        )
        .expect("identification done");
    let registry = managed.library.core.live_registry();
    managed
        .library
        .coordinator
        .request_run(&droppedneedle_library::scan::ScanRequest {
            kind: droppedneedle_library::scan::ScanKind::Incremental,
            trigger: droppedneedle_library::scan::ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    assert!(
        !step_at(&managed, &mut settle, start, 2.0 * SETTLE_SECONDS + 20.0).await,
        "a running scan holds every album back"
    );
    assert!(queued(&identifying));
    drain_scans(&managed.library).await;
    assert!(
        step_at(&managed, &mut settle, start, 2.0 * SETTLE_SECONDS + 30.0).await,
        "the album is looked at once the scan is done"
    );
}

/// One automatic step at `start + at`, after an identification seal for
/// some other album bumped the catalog revision.
async fn step_at(managed: &ManagedLibrary, settle: &mut Settle, start: f64, at: f64) -> bool {
    managed
        .db()
        .execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
            [],
        )
        .expect("revision bumped");
    let management = managed.management();
    let mut state = std::mem::take(settle);
    let (state, looked) = tokio::task::spawn_blocking(move || {
        let looked = management.automatic_step_at(&mut state, start + at);
        (state, looked)
    })
    .await
    .expect("worker joins");
    *settle = state;
    looked.expect("automatic step")
}

/// An album in a folder set to Local metadata keeps its own tags: with an
/// edition someone chose and management on for its library folder,
/// neither a person's preview nor automatic management plans a write
/// there, and both say why. The same album in an ordinary folder is
/// managed.
#[tokio::test]
async fn management_leaves_a_folder_that_keeps_its_own_tags() {
    use droppedneedle_kernel::runtime_config::secret_sections::{
        IdentificationPolicy, LibraryPathRule, TypedLibrary,
    };
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};
    let managed = ManagedLibrary::start("lib-own-tags", |_| {}).await;
    let mut albums = Vec::new();
    for folder in ["own", "plain"] {
        let (album, tracks) = managed.add_album(folder, 1).await;
        let current: String = managed
            .db()
            .query_row(
                "SELECT release_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1",
                [&album],
                |row| row.get(0),
            )
            .expect("edition");
        // The first album has REL_1 and REL_2; a later one's two editions
        // differ only in their second id group.
        let other = match current.as_str() {
            REL_1 => REL_2.to_owned(),
            REL_2 => REL_1.to_owned(),
            _ if current.contains("-0000-") => current.replacen("-0000-", "-0001-", 1),
            _ => current.replacen("-0001-", "-0000-", 1),
        };
        crate::common::library::operations(&managed.library)
            .choose_edition(&album, &other, Some("admin"))
            .await
            .expect("edition chosen");
        albums.push((album, tracks));
    }
    let [(own, own_tracks), (plain, plain_tracks)] =
        <[_; 2]>::try_from(albums).expect("two albums");
    managed
        .library
        .settings
        .store()
        .update_secret_blocking::<TypedLibrary, String, _>(|stored| {
            let mut library = stored.into_inner();
            for root in &mut library.library_roots {
                root.rules.push(LibraryPathRule {
                    id: "own-rule".to_owned(),
                    relative_path: "own".to_owned(),
                    policy: IdentificationPolicy::LocalMetadata,
                });
            }
            Ok(library)
        })
        .expect("path rule saved");
    managed.library.core.refresh_registry();
    managed.activate().await;
    let own_file = managed.path(&own_tracks[0]);
    let own_before = file_sha256(&own_file);
    let plain_before = file_sha256(&managed.path(&plain_tracks[0]));

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&own], "own-preview")
        .await;
    assert!(!items.is_empty());
    for item in &items {
        assert_eq!(item.eligibility, Eligibility::Blocked);
        assert_eq!(
            item.reason().map(|reason| reason.code.as_str()),
            Some("LOCAL_METADATA")
        );
    }
    // A person's own edit of a tag there is the folder's own tags: it plans.
    let management = managed.management();
    let edit = management
        .create_tag_edit(
            droppedneedle_library::management::tag_edit::TagEditRequest {
                local_track_id: own_tracks[0].clone(),
                mode: droppedneedle_library::management::models::TagEditMode::SaveOverride,
                expected_settings_revision: managed.settings_revision(),
                expected_policy_revision: managed
                    .library
                    .core
                    .policy_revision()
                    .expect("policy revision"),
                fields: vec![("title".to_owned(), serde_json::json!("Typo Fixed"))],
                idempotency_key: Some("own-edit".to_owned()),
            },
            "admin",
        )
        .expect("edit preview");
    assert!(
        management_step(&managed.library).await,
        "edit planning runs"
    );
    let items = management
        .items(&edit.job_id, -1, 100, &Default::default())
        .expect("items")
        .items;
    assert!(
        items.iter().any(|item| item.eligibility.writes()),
        "a tag edit in the folder still plans: {:?}",
        items.iter().map(|item| item.reason()).collect::<Vec<_>>()
    );
    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&plain], "plain-preview")
        .await;
    assert!(
        items
            .iter()
            .all(|item| item.eligibility.writes() && item.diff.changes_anything()),
        "the ordinary folder's album is planned"
    );

    managed.look(&own).await;
    assert!(
        managed
            .skipped_codes()
            .contains(&(own.clone(), "LOCAL_METADATA".to_owned()))
    );
    assert_eq!(managed.automatic_runs().len(), 0, "no run for the album");
    assert_eq!(file_sha256(&own_file), own_before, "its file is untouched");

    managed.look(&plain).await;
    let runs = managed.automatic_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, "succeeded");
    assert_ne!(
        file_sha256(&managed.path(&plain_tracks[0])),
        plain_before,
        "the ordinary folder's album is written"
    );
}

fn title_edit(value: &str) -> droppedneedle_library::tags::TagEdit {
    droppedneedle_library::tags::TagEdit::verbatim(
        droppedneedle_library::tags::TagField::Title,
        vec![value.to_owned()],
    )
}

/// A file the app wrote that is retagged outside it is the user's: the
/// next automatic run writes nothing, marks the album as taken over and
/// parks it, and never touches the album's match. The app's own write
/// never does that, and neither does a hand edit made before the app
/// ever wrote the file. A person's preview of the whole folder then
/// leaves the album alone, and a preview of the album itself warns.
#[tokio::test]
async fn a_hand_retag_takes_the_album_over() {
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};
    use droppedneedle_library::tags::{TagField, read_fields};

    let managed = ManagedLibrary::start("lib-takeover", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    let (other, _) = managed.add_album("second", 1).await;
    managed.activate().await;
    let track = &tracks[0];
    let title = |path: &Path| read_fields(path).expect("tags read")[&TagField::Title].clone();

    // The app has never written this file: its first run goes ahead.
    managed.edit_outside(track, title_edit("Before")).await;
    managed.look(&album).await;
    let file = managed.path(track);
    assert_eq!(
        title(&file),
        vec!["first 1".to_owned()],
        "the edition's title"
    );
    assert_eq!(managed.mark(&album), None);

    // The scan sees the app's own write as a changed file. The worker
    // looks and finds the album as it left it.
    let runs = managed.automatic_runs().len();
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert_eq!(
        managed.mark(&album),
        None,
        "the app's own write never marks"
    );
    assert_eq!(managed.automatic_runs().len(), runs, "no new run");

    // Retagged by hand: the scan queues the album, and the worker steps
    // back from it.
    let identity = |album: &str| -> (String, String, i64) {
        managed
            .db()
            .query_row(
                "SELECT release_mbid, decision_source, row_revision \
                 FROM local_album_external_identities WHERE local_album_id = ?1",
                [album],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("match row")
    };
    let matched = identity(&album);
    managed.edit_outside(track, title_edit("By Hand")).await;
    let edited = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(
        managed.automatic_runs().last(),
        Some(&(
            "succeeded".to_owned(),
            Some("FILES_CHANGED_OUTSIDE".to_owned())
        ))
    );
    assert_eq!(file_sha256(&file), edited, "nothing written");
    assert_eq!(identity(&album), matched, "the match is the same row");
    let skipped = managed.management().automatic_skipped(10).expect("skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].album_id, album);
    assert_eq!(skipped[0].reason.code, "TAKEN_OVER");
    let stepped_back = "SELECT COUNT(*) FROM library_catalog_actions \
                        WHERE action_kind = 'management_stepped_back'";
    assert_eq!(managed.count(stepped_back), 1);

    // Scanned and looked at again: still nothing written, said once.
    let runs = managed.automatic_runs().len();
    rescan(&managed.library).await;
    managed.look(&album).await;
    assert_eq!(file_sha256(&file), edited);
    assert_eq!(managed.automatic_runs().len(), runs);
    assert_eq!(managed.count(stepped_back), 1);

    // A preview of more than this one album leaves it alone and plans the
    // other one as before: the whole folder, or the two albums picked
    // from a list.
    assert_ne!(album, other);
    let whole_folder = managed
        .preview(SelectionKind::Roots, &["music"], "wide")
        .await;
    let both_picked = managed
        .preview(SelectionKind::Albums, &[&album, &other], "both")
        .await;
    for (_, items) in [whole_folder, both_picked] {
        for item in &items {
            if item.album_id == album {
                assert_eq!(item.eligibility, Eligibility::Blocked);
                assert_eq!(item.reason().expect("a reason").code, "TAKEN_OVER");
                assert!(item.fingerprint.is_empty(), "blocked before it was read");
            } else {
                assert_eq!(item.album_id, other);
                assert!(item.eligibility.writes(), "{:?}", item.facts);
            }
        }
        assert_eq!(items.len(), 2);
    }

    // A preview of the album itself plans, and says what applying does.
    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "own")
        .await;
    assert!(items[0].eligibility.writes(), "{:?}", items[0].facts);
    assert!(items[0].diff.tags_changed);
    assert!(
        items[0]
            .facts
            .warnings
            .iter()
            .any(|warning| warning.contains("Applying this preview overwrites those changes")),
        "{:?}",
        items[0].facts.warnings
    );
    assert_eq!(file_sha256(&file), edited);
}

/// What the app does not manage is not a takeover: a tag outside the
/// profile's fields is left in the file and recorded, a file moved by hand
/// is put back where the naming rules say, and a tag edit made in the app
/// is the app's own write.
#[tokio::test]
async fn unmanaged_changes_do_not_take_the_album_over() {
    use droppedneedle_library::management::models::TagEditMode;
    use droppedneedle_library::management::tag_edit::TagEditRequest;
    use droppedneedle_library::tags::TagEdit;
    use droppedneedle_library::tags::save::read_document;

    let managed = ManagedLibrary::start("lib-takeover-unmanaged", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    managed.activate().await;
    let track = &tracks[0];
    managed.look(&album).await;
    let file = managed.path(track);

    // A tag the profile does not manage.
    managed
        .edit_outside(
            track,
            TagEdit::native("vorbis:MOOD", vec!["Calm".to_owned()]),
        )
        .await;
    let edited = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(file_sha256(&file), edited, "the tag is left in the file");
    assert_eq!(managed.recorded(track), Some(edited), "and recorded");
    let mood = |path: &Path| {
        read_document(path)
            .expect("tags read")
            .native
            .get("vorbis:MOOD")
            .cloned()
    };

    // The edition changes in the app: the worker writes the file from it,
    // and the tag it does not manage is still there.
    let chosen: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    let other = if chosen == REL_1 { REL_2 } else { REL_1 };
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, other, Some("admin"))
        .await
        .expect("edition chosen");
    managed.look(&album).await;
    let last = managed.automatic_runs().pop().expect("a run");
    assert_eq!(last.0, "succeeded");
    assert_ne!(last.1.as_deref(), Some("MANAGEMENT_UNCHANGED"), "it wrote");
    let file = managed.path(track);
    assert_eq!(mood(&file), Some(vec!["Calm".to_owned()]));
    assert_eq!(managed.recorded(track), Some(file_sha256(&file)));
    assert_eq!(managed.mark(&album), None);

    // Moved by hand, tags untouched.
    let elsewhere = managed.music.join("elsewhere/moved.flac");
    std::fs::create_dir_all(elsewhere.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&file, &elsewhere).expect("moved by hand");
    rescan(&managed.library).await;
    assert_eq!(managed.path(track), elsewhere, "the track follows the file");
    managed.look(&album).await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(managed.path(track), file, "put back by the naming rules");
    assert!(file.exists() && !elsewhere.exists());
    assert_eq!(mood(&file), Some(vec!["Calm".to_owned()]));

    // A tag edit made in the app.
    let management = managed.management();
    let edit = management
        .create_tag_edit(
            TagEditRequest {
                local_track_id: track.clone(),
                mode: TagEditMode::SaveOverride,
                expected_settings_revision: managed.settings_revision(),
                expected_policy_revision: managed
                    .library
                    .core
                    .policy_revision()
                    .expect("policy revision"),
                fields: vec![("title".to_owned(), serde_json::json!("Edited In App"))],
                idempotency_key: Some("edit".to_owned()),
            },
            "admin",
        )
        .expect("edit preview");
    assert!(
        management_step(&managed.library).await,
        "edit planning runs"
    );
    let applied = management_apply(&managed.library, &edit).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    rescan(&managed.library).await;
    managed.look(&album).await;
    assert_eq!(managed.mark(&album), None, "the app's own edit never marks");
    let file = managed.path(track);
    assert_eq!(managed.recorded(track), Some(file_sha256(&file)));
    assert_eq!(mood(&file), Some(vec!["Calm".to_owned()]));
}

/// The MP3 fixture's main text frames as a clean ID3v2.3 tag over its audio,
/// written to `path`. The committed v2.3 fixture carries v2.4-only
/// frames, so the writer refuses it.
fn clean_v23_mp3(path: &Path) -> String {
    use crate::library_tags::{id3_frames, id3_strings, id3v23_raw, mp3_audio_tail};
    const KEPT: &[&str] = &["TIT2", "TPE1", "TALB", "TPE2", "TRCK", "TPOS", "TXXX"];
    let source = fixtures_dir().join("management_full.mp3");
    let frames: Vec<(String, Vec<u8>)> = id3_frames(&source)
        .1
        .into_iter()
        .filter(|(id, _)| KEPT.contains(&id.as_str()))
        .map(|(id, payload)| {
            let mut utf16 = vec![1];
            for text in id3_strings(&payload) {
                utf16.extend_from_slice(&[0xFF, 0xFE]);
                for unit in text.encode_utf16() {
                    utf16.extend_from_slice(&unit.to_le_bytes());
                }
                utf16.extend_from_slice(&[0, 0]);
            }
            (id, utf16)
        })
        .collect();
    let mut refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    refs.push(("TYER", b"\x002024"));
    let mut bytes = id3v23_raw(&refs);
    bytes.extend_from_slice(mp3_audio_tail(
        &std::fs::read(&source).expect("fixture reads"),
    ));
    std::fs::write(path, bytes).expect("v2.3 file written");
    path.to_string_lossy().into_owned()
}

/// An edition with every fact the split fields write: sort names, a disc
/// title, a first release date, a type, and an ISRC per track.
fn full_edition(release: &mut Release) {
    use droppedneedle_library::matching::model::ReleaseMedium;
    for artist in &mut release.artists {
        artist.sort_name = Some(format!("{}, Sorted", artist.name));
    }
    release.original_date = Some("1999".to_owned());
    release.primary_type = Some("Album".to_owned());
    release.media = vec![ReleaseMedium {
        position: 1,
        format: Some("CD".to_owned()),
        title: Some("Disc Title".to_owned()),
        track_count: release.tracks.len() as u32,
    }];
    for (index, track) in release.tracks.iter_mut().enumerate() {
        track.isrcs = vec![isrc(index)];
    }
}

fn isrc(index: usize) -> String {
    format!("GBAAA99{index:05}")
}

/// A field's values read from the file's bytes, in its format's home.
fn native_values(path: &Path, field: droppedneedle_library::tags::TagField) -> Vec<String> {
    use crate::library_tags::{
        id3_comments, id3_text_values, id3_txxx_values, mp4_freeform, mp4_text, vorbis_key_values,
    };
    use droppedneedle_library::tags::fields::{Id3Target, Mp4Target};
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .expect("extension");
    let id3 = |target: Id3Target| match target {
        Id3Target::Text(id) => id3_text_values(path, id),
        Id3Target::User(description) => id3_txxx_values(path, description),
        Id3Target::Comment => id3_comments(path)
            .into_iter()
            .filter(|(_, description, _)| description.is_empty())
            .map(|(_, _, text)| text)
            .collect(),
        other => panic!("no byte reader for {other:?}"),
    };
    match extension {
        "flac" | "ogg" | "opus" => vorbis_key_values(path, field.vorbis_keys()[0]),
        "mp3" => {
            let v23 = std::fs::read(path).expect("file reads")[3] == 3;
            id3(field.id3(v23))
        }
        "m4a" => match field.mp4().expect("an MP4 home") {
            Mp4Target::Text(kind) => mp4_text(path, &kind),
            Mp4Target::Freeform(name) => mp4_freeform(path, name),
            other => panic!("no byte reader for {other:?}"),
        },
        other => panic!("no byte reader for {other}"),
    }
}

/// An MP3 whose ID3v2.3 tag also holds ID3v2.4 frames is managed like any
/// other file: its preview warns that it is saved as ID3v2.4, Apply
/// writes it so, and Undo and Restore originals put every value back in
/// the ID3v2.4 tag.
#[tokio::test]
async fn a_mixed_id3v23_file_is_saved_as_v24() {
    use crate::library_tags::{id3_frames, id3_txxx_values, mp3_audio_tail};
    use droppedneedle_library::management::models::{Eligibility, Selection, SelectionKind};
    use droppedneedle_library::management::restore::RestoreRequest;
    use droppedneedle_library::tags::read_fields;

    let managed = ManagedLibrary::start("lib-mixed-v23", |_| {}).await;
    let (album, tracks) = managed
        .add_untouched_album("mixed", "management_full_v23.mp3")
        .await;
    let file = || managed.path(&tracks[0]);
    let original = read_fields(&file()).expect("tags read");
    let audio = mp3_audio_tail(&std::fs::read(file()).expect("file reads")).to_vec();
    let saved_as_v24 = |what: &str| {
        assert_eq!(id3_frames(&file()).0, 4, "{what}");
        let bytes = std::fs::read(file()).expect("file reads");
        assert_eq!(mp3_audio_tail(&bytes), audio, "{what}: the audio");
        assert_eq!(
            id3_txxx_values(&file(), "CUSTOM_KEEP"),
            vec!["opaque local value".to_owned()],
            "{what}: an unrelated tag"
        );
    };

    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&album], "mixed")
        .await;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].eligibility,
        Eligibility::Warning,
        "{:?}",
        items[0].facts
    );
    assert!(items[0].diff.tags_changed);
    assert!(
        items[0].facts.warnings.iter().any(|warning| warning
            == "Its ID3v2.3 tag also holds newer ID3v2.4 tags, so it is saved as ID3v2.4."),
        "{:?}",
        items[0].facts.warnings
    );
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    saved_as_v24("applied");

    let management = managed.management();
    let detail = management.detail(&handle.job_id).expect("detail");
    let undo = management
        .undo_preview(&handle.job_id, detail.job.row_revision, "undo", "admin")
        .expect("undo preview");
    assert!(
        management_step(&managed.library).await,
        "undo planning runs"
    );
    let undone = management_apply(&managed.library, &undo).await;
    assert_eq!(undone.job.state.as_str(), "succeeded");
    assert_eq!(read_fields(&file()).expect("tags read"), original, "undone");
    saved_as_v24("undone");

    let (handle, _) = managed
        .preview(SelectionKind::Albums, &[&album], "again")
        .await;
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    let restore = management
        .create_restore_preview(
            RestoreRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album.clone()],
                    catalog_filter: None,
                },
                expected_settings_revision: managed.settings_revision(),
                expected_policy_revision: managed
                    .library
                    .core
                    .policy_revision()
                    .expect("policy revision"),
                idempotency_key: "restore".to_owned(),
            },
            "admin",
        )
        .expect("restore preview");
    assert!(
        management_step(&managed.library).await,
        "restore planning runs"
    );
    let restored = management_apply(&managed.library, &restore).await;
    assert_eq!(
        restored.job.state.as_str(),
        "succeeded",
        "{:?}",
        restored.job.terminal_code
    );
    assert_eq!(
        read_fields(&file()).expect("tags read"),
        original,
        "restored"
    );
    saved_as_v24("restored");
}

/// A scrub's preview says, per file, how many tags it keeps as they are
/// because undo could not put them back exactly, and why: an ID3 frame
/// over 1 MB, an MP4 item that is not text. A file no save could keep
/// whole is left out of the run with the reason: a FLAC spelling a tag
/// two ways, and an MP3 holding two identical pictures in a run that
/// replaces the art.
#[tokio::test]
async fn a_scrub_preview_names_the_tags_it_keeps() {
    use crate::library_tags::{flac_inject_pairs, id3_inject_frames};
    use droppedneedle_library::management::enrich::{Cover, ScriptedEnrichment};
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};

    let mut cover = Vec::new();
    image::RgbImage::from_pixel(8, 8, image::Rgb([30, 30, 200]))
        .write_to(
            &mut std::io::Cursor::new(&mut cover),
            image::ImageFormat::Png,
        )
        .expect("png");
    let managed = ManagedLibrary::scripted(
        "lib-kept-as-is",
        |profile| {
            profile.metadata.scrub_unmanaged_tags = true;
            profile.artwork.embedded_enabled = true;
        },
        Arc::new(ScriptedEnrichment {
            listenbrainz: None,
            lyrics: None,
            cover: Some(Cover {
                bytes: cover,
                mime: "image/png".to_owned(),
            }),
            ..Default::default()
        }),
    )
    .await;
    let (kept, kept_tracks) = managed
        .add_album_from(
            "kept",
            &["management_full.mp3", "management_full.m4a"],
            |_| {},
        )
        .await;
    let (spelled, spelled_tracks) = managed
        .add_album_from("spelled", &["management_full.flac"], |_| {})
        .await;
    let (doubled, doubled_tracks) = managed
        .add_album_from("doubled", &["management_full.mp3"], |_| {})
        .await;
    let mut geob = b"\x00application/octet-stream\x00blob.bin\x00large\x00".to_vec();
    geob.extend((0..(1 << 20) + 4096).map(|index| (index % 251) as u8));
    id3_inject_frames(&managed.path(&kept_tracks[0]), &[("GEOB", &geob)]);
    flac_inject_pairs(
        &managed.path(&spelled_tracks[0]),
        &[("Mood", "Calm"), ("MOOD", "Bright")],
    );
    let picture = b"\0image/png\0\x03\0abc".as_slice();
    id3_inject_frames(
        &managed.path(&doubled_tracks[0]),
        &[("APIC", picture), ("APIC", picture)],
    );
    rescan(&managed.library).await;

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&kept, &spelled, &doubled], "kept")
        .await;
    let item = |track: &str| {
        items
            .iter()
            .find(|item| item.track_id.as_deref() == Some(track))
            .expect("item for the track")
    };
    for (track, note) in [
        (
            &kept_tracks[0],
            "1 tag kept as it is, since undo could not restore it exactly: GEOB (over 1 MB).",
        ),
        (
            &kept_tracks[1],
            "1 tag kept as it is, since undo could not restore it exactly: stik (not text).",
        ),
    ] {
        let item = item(track);
        assert_eq!(item.eligibility, Eligibility::Warning, "{:?}", item.facts);
        assert!(
            item.facts.warnings.iter().any(|warning| warning == note),
            "{:?}",
            item.facts.warnings
        );
    }
    for (track, words) in [
        (
            &spelled_tracks[0],
            "The file spells its MOOD tag more than one way (Mood, MOOD), so saving its tags would merge them.",
        ),
        (&doubled_tracks[0], "two APIC frames"),
    ] {
        let item = item(track);
        assert_eq!(item.eligibility, Eligibility::Blocked, "{:?}", item.facts);
        let reason = item.reason().expect("a reason");
        assert_eq!(reason.code, "WRITE_FAILED");
        assert!(reason.message.contains(words), "{}", reason.message);
    }
}

/// The Picard profile applied to one album in every writable format
/// writes each field in that format's own home: artist sort, album
/// artists, ISRC, disc subtitle, original date and release type. Comment,
/// set to replace with clear, removes the file's comment and leaves an
/// iTunNORM `COMM` alone. Turning artist sort off stops that tag only.
#[tokio::test]
async fn the_picard_profile_writes_its_fields_in_every_format() {
    use crate::library_tags::{id3_frames, id3_inject_frames};
    use droppedneedle_kernel::runtime_config::sections::{FieldMode, LibraryManagement};
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let managed = ManagedLibrary::start("lib-fields-formats", |profile| {
        for field in &mut profile.metadata.fields {
            if field.field == "comment" {
                field.clear_when_canonical_missing = true;
            }
        }
    })
    .await;
    let v23 = clean_v23_mp3(&managed.scratch.to_path_buf().join("v23.mp3"));
    let fixtures = [
        "management_full.flac",
        "management_full.mp3",
        v23.as_str(),
        "management_full.ogg",
        "management_full.opus",
        "management_full.m4a",
    ];
    let (album, tracks) = managed
        .add_album_from("formats", &fixtures, full_edition)
        .await;
    let itunnorm = b"\x00engiTunNORM\x00 00000A1B 00000C2D".to_vec();
    for track in &tracks {
        let file = managed.path(track);
        save_tags(
            &file,
            &[TagEdit::new(TagField::Comment, vec!["Ripped".to_owned()])],
        )
        .expect("comment planted");
        if std::fs::read(&file).expect("file reads")[..4] == *b"ID3\x04" {
            id3_inject_frames(&file, &[("COMM", &itunnorm)]);
        }
    }
    rescan(&managed.library).await;
    let apply = |key: &'static str| {
        let managed = &managed;
        let album = album.clone();
        async move {
            let (handle, items) = managed.preview(SelectionKind::Albums, &[&album], key).await;
            assert_eq!(items.len(), 6);
            for item in &items {
                assert!(item.eligibility.writes(), "{:?}", item.facts);
            }
            let applied = management_apply(&managed.library, &handle).await;
            assert_eq!(applied.job.state.as_str(), "succeeded");
        }
    };
    apply("every-field").await;

    let sort = vec!["Alpha, Sorted".to_owned()];
    for (index, track) in tracks.iter().enumerate() {
        let file = managed.path(track);
        let name = file.display();
        assert_eq!(native_values(&file, TagField::ArtistSort), sort, "{name}");
        assert_eq!(
            native_values(&file, TagField::AlbumArtists),
            vec!["Alpha".to_owned()],
            "{name}"
        );
        assert_eq!(
            native_values(&file, TagField::Isrc),
            vec![isrc(index)],
            "{name}"
        );
        assert_eq!(
            native_values(&file, TagField::DiscSubtitle),
            vec!["Disc Title".to_owned()],
            "{name}"
        );
        assert_eq!(
            native_values(&file, TagField::OriginalDate),
            vec!["1999".to_owned()],
            "{name}"
        );
        assert_eq!(
            native_values(&file, TagField::ReleaseType),
            vec!["album".to_owned()],
            "{name}"
        );
        assert!(
            native_values(&file, TagField::Comment).is_empty(),
            "{name}: the comment is cleared"
        );
        if std::fs::read(&file).expect("file reads")[..4] == *b"ID3\x04" {
            let comments: Vec<Vec<u8>> = id3_frames(&file)
                .1
                .into_iter()
                .filter(|(id, _)| id == "COMM")
                .map(|(_, payload)| payload)
                .collect();
            assert_eq!(comments, vec![itunnorm.clone()], "{name}: iTunNORM stays");
        }
    }

    // Artist sort off: a hand edit to it stays, while one to the ISRC
    // beside it is put back.
    let mut settings: LibraryManagement = managed.library.settings.store().get().expect("settings");
    for profile in &mut settings.profiles {
        for field in &mut profile.metadata.fields {
            if field.field == "artist_sort" {
                field.mode = FieldMode::Disabled;
            }
        }
    }
    managed
        .library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");
    let by_hand = vec!["By Hand".to_owned()];
    for track in &tracks {
        save_tags(
            &managed.path(track),
            &[
                TagEdit::new(TagField::ArtistSort, by_hand.clone()),
                TagEdit::new(TagField::Isrc, vec!["XXHAND0000001".to_owned()]),
            ],
        )
        .expect("edited by hand");
    }
    rescan(&managed.library).await;
    apply("no-artist-sort").await;
    for (index, track) in tracks.iter().enumerate() {
        let file = managed.path(track);
        let name = file.display();
        assert_eq!(
            native_values(&file, TagField::ArtistSort),
            by_hand,
            "{name}"
        );
        assert_eq!(
            native_values(&file, TagField::Isrc),
            vec![isrc(index)],
            "{name}"
        );
    }
}

/// An ISRC edited by hand after the app wrote the file is the user's: the
/// worker looks, marks the album and writes nothing.
#[tokio::test]
async fn a_hand_edited_isrc_takes_the_album_over() {
    use droppedneedle_library::tags::{TagEdit, TagField, read_fields};

    let managed = ManagedLibrary::start("lib-takeover-isrc", |_| {}).await;
    let (album, tracks) = managed
        .add_album_from("first", &["management_full.flac"], full_edition)
        .await;
    managed.activate().await;
    let track = &tracks[0];
    managed.look(&album).await;
    let file = managed.path(track);
    assert_eq!(
        read_fields(&file).expect("tags read")[&TagField::Isrc],
        vec![isrc(0)],
        "the app wrote the ISRC"
    );
    assert_eq!(managed.mark(&album), None);

    managed
        .edit_outside(
            track,
            TagEdit::verbatim(TagField::Isrc, vec!["XXHAND0000001".to_owned()]),
        )
        .await;
    let edited = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(file_sha256(&file), edited, "nothing written");
}

/// A hand edit counts even when another file of the album cannot be
/// planned: the run ends with the album marked, and no preview waits for
/// a person to apply over the edit.
#[tokio::test]
async fn a_blocked_file_does_not_hide_a_hand_edit() {
    let managed = ManagedLibrary::start("lib-takeover-blocked", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 2).await;
    managed.activate().await;
    managed.look(&album).await;
    assert_eq!(managed.mark(&album), None);

    // The second file's match names a track the edition does not have.
    managed
        .db()
        .execute(
            "UPDATE local_track_external_identities SET release_track_mbid = 'no-such-track', \
             row_revision = row_revision + 1 WHERE local_track_id = ?1",
            [&tracks[1]],
        )
        .expect("mapping broken");
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;
    let edited = file_sha256(&managed.path(&tracks[0]));
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(
        managed.automatic_runs().last(),
        Some(&(
            "succeeded".to_owned(),
            Some("FILES_CHANGED_OUTSIDE".to_owned())
        ))
    );
    assert!(
        managed
            .automatic_runs()
            .iter()
            .all(|(state, _)| state == "succeeded"),
        "no automatic preview waits on the operations page"
    );
    assert_eq!(file_sha256(&managed.path(&tracks[0])), edited);
}

/// A file v2 tagged is recognised by the record v2 left: untouched, v3
/// goes on managing it; edited by hand since, the album is the user's.
/// A v2 row that holds no file hash is no record at all.
#[tokio::test]
async fn a_file_v2_wrote_is_recognised() {
    use droppedneedle_library::publish::record::v2_path_revision;

    let managed = ManagedLibrary::start("lib-takeover-v2", |_| {}).await;
    let (untouched, untouched_tracks) = managed.add_album("untouched", 1).await;
    let (edited, edited_tracks) = managed.add_album("edited", 1).await;
    let (recycled, recycled_tracks) = managed.add_album("recycled", 1).await;
    managed.activate().await;
    // What the upgrade carries over: v2's value, its outcome and no job.
    let v2_row = |track: &str, outcome: &str| {
        let file = managed.path(track);
        let relative = file
            .strip_prefix(&managed.music)
            .expect("under the root")
            .to_string_lossy()
            .into_owned();
        managed
            .db()
            .execute(
                "INSERT INTO library_track_management_state (local_track_id, managed_root_id, \
                 managed_path_revision, last_outcome, last_managed_at) \
                 VALUES (?1, 'music', ?2, ?3, 1)",
                rusqlite::params![
                    track,
                    v2_path_revision("music", &relative, &file_sha256(&file)),
                    outcome
                ],
            )
            .expect("v2 row");
    };
    v2_row(&untouched_tracks[0], "succeeded");
    v2_row(&edited_tracks[0], "succeeded");
    v2_row(&recycled_tracks[0], "recycled");

    let before = managed.path(&untouched_tracks[0]);
    managed.look(&untouched).await;
    let after = managed.path(&untouched_tracks[0]);
    assert_ne!(after, before, "the first run goes ahead");
    assert_eq!(managed.mark(&untouched), None);
    assert_eq!(
        managed.recorded(&untouched_tracks[0]),
        Some(file_sha256(&after))
    );

    managed
        .edit_outside(&edited_tracks[0], title_edit("By Hand"))
        .await;
    let file = managed.path(&edited_tracks[0]);
    let bytes = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&edited).as_deref(), Some("files_changed"));
    assert_eq!(managed.path(&edited_tracks[0]), file);
    assert_eq!(file_sha256(&file), bytes, "nothing written");

    managed
        .edit_outside(&recycled_tracks[0], title_edit("By Hand"))
        .await;
    let before = managed.path(&recycled_tracks[0]);
    managed.look(&recycled).await;
    assert_eq!(managed.mark(&recycled), None, "no record, so no takeover");
    assert_ne!(
        managed.path(&recycled_tracks[0]),
        before,
        "the run goes ahead"
    );
}

/// Undoing a profile run in a folder that is managed without asking
/// leaves the album unmanaged, whoever started the run: otherwise the
/// worker would do the same thing again half a minute later.
#[tokio::test]
async fn an_undone_run_is_not_done_again() {
    use droppedneedle_library::management::models::SelectionKind;

    let managed = ManagedLibrary::start("lib-takeover-undo", |_| {}).await;
    let (by_worker, worker_tracks) = managed.add_album("first", 1).await;
    let (by_person, person_tracks) = managed.add_album("second", 1).await;
    managed.activate().await;
    let management = managed.management();
    let undo = |job_id: String, key: &'static str| {
        let management = management.clone();
        let library = managed.library.clone();
        async move {
            let detail = management.detail(&job_id).expect("detail");
            let undo = management
                .undo_preview(&job_id, detail.job.row_revision, key, "admin")
                .expect("undo preview");
            assert!(management_step(&library).await, "undo planning runs");
            let undone = management_apply(&library, &undo).await;
            assert_eq!(undone.job.state.as_str(), "succeeded");
        }
    };

    // A run the worker started.
    managed.look(&by_worker).await;
    let (history, _) = management.history(Default::default()).expect("history");
    let run = history
        .iter()
        .find(|item| item.origin == "scan_discovered")
        .expect("an automatic run")
        .job
        .id
        .clone();
    undo(run, "undo-worker").await;
    let file = managed.path(&worker_tracks[0]);
    let bytes = file_sha256(&file);
    rescan(&managed.library).await;
    managed.look(&by_worker).await;
    assert_eq!(
        managed.path(&worker_tracks[0]),
        file,
        "the worker moves nothing"
    );
    assert_eq!(file_sha256(&file), bytes, "the worker writes nothing");
    assert_eq!(managed.mark(&by_worker).as_deref(), Some("run_undone"));

    // A run a person started, before the worker got to the album.
    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&by_person], "by-person")
        .await;
    assert!(
        items[0].eligibility.writes() && items[0].diff.changes_anything(),
        "{:?} {:?}",
        items[0].facts,
        items[0].diff
    );
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    undo(handle.job_id.clone(), "undo-person").await;
    let file = managed.path(&person_tracks[0]);
    let bytes = file_sha256(&file);
    rescan(&managed.library).await;
    managed.look(&by_person).await;
    assert_eq!(
        managed.path(&person_tracks[0]),
        file,
        "the worker moves nothing"
    );
    assert_eq!(file_sha256(&file), bytes, "the worker writes nothing");
    assert_eq!(managed.mark(&by_person).as_deref(), Some("run_undone"));
}

/// Undoing one bundle of an automatic run (the route that undoes a single
/// bundle) leaves the album unmanaged as well, so the run is not redone.
#[tokio::test]
async fn an_undone_bundle_is_not_done_again() {
    let managed = ManagedLibrary::start("lib-takeover-undo-bundle", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    managed.activate().await;
    let planted = managed.path(&tracks[0]);
    managed.look(&album).await;
    assert_ne!(managed.path(&tracks[0]), planted);
    let bundle: String = managed
        .db()
        .query_row(
            "SELECT w.bundle_id FROM library_operation_work w \
             JOIN library_management_job_snapshots s ON s.job_id = w.job_id \
             WHERE s.origin = 'scan_discovered' AND w.bundle_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("the run's bundle");

    crate::common::library::services(&managed.library)
        .management
        .undo_bundle(&bundle, None)
        .expect("bundle undone");
    assert_eq!(managed.path(&tracks[0]), planted);
    let bytes = file_sha256(&planted);
    rescan(&managed.library).await;
    managed.look(&album).await;
    assert_eq!(managed.path(&tracks[0]), planted, "nothing is redone");
    assert_eq!(file_sha256(&planted), bytes);
    assert_eq!(managed.mark(&album).as_deref(), Some("run_undone"));
}

/// A picture replaced by hand takes the album over where the profile
/// manages pictures, and nowhere else.
#[tokio::test]
async fn a_replaced_picture_counts_only_where_pictures_are_managed() {
    use droppedneedle_library::management::enrich::{Cover, ScriptedEnrichment};
    use droppedneedle_library::tags::save::{PictureSpec, save_tags_with_pictures};

    let png = |colour: [u8; 3]| {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(8, 8, image::Rgb(colour))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        bytes
    };
    for (tag, artwork) in [("lib-takeover-art", true), ("lib-takeover-no-art", false)] {
        let managed = ManagedLibrary::scripted(
            tag,
            |profile| {
                profile.artwork.embedded_enabled = artwork;
                // No credits are scripted here.
                for field in &mut profile.metadata.fields {
                    if droppedneedle_library::management::enrich::CREDIT_FIELDS
                        .iter()
                        .any(|(name, _)| *name == field.field)
                    {
                        field.mode =
                            droppedneedle_kernel::runtime_config::sections::FieldMode::Disabled;
                    }
                }
            },
            Arc::new(ScriptedEnrichment {
                listenbrainz: None,
                lyrics: None,
                cover: Some(Cover {
                    bytes: png([30, 30, 200]),
                    mime: "image/png".to_owned(),
                }),
                ..Default::default()
            }),
        )
        .await;
        let (album, tracks) = managed.add_album("first", 1).await;
        managed.activate().await;
        managed.look(&album).await;
        let file = managed.path(&tracks[0]);

        save_tags_with_pictures(
            &file,
            &[],
            Some(&[PictureSpec {
                kind: 3,
                mime: "image/png".to_owned(),
                description: String::new(),
                data: png([200, 30, 30]),
                info: None,
            }]),
        )
        .expect("picture replaced outside the app");
        let edited = file_sha256(&file);
        rescan(&managed.library).await;
        managed.worker_looks().await;
        assert_eq!(
            managed.mark(&album).as_deref(),
            artwork.then_some("files_changed"),
            "artwork managed: {artwork}"
        );
        assert_eq!(file_sha256(&file), edited, "the picture is left alone");
    }
}

/// The app's step back goes with the tracks: a merge by hand carries it to
/// the album that stays, and a split gives it to the album made.
#[tokio::test]
async fn a_merge_and_a_split_carry_the_step_back() {
    let rig = Rig::start("lib-takeover-membership").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "a/02.flac", "flac_full_02.flac");
    plant(&music, "b/01.flac", "flac_full_01.flac");
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    app.scan_library().await;
    let store = &rig.scan_store;
    let a2 = store.track_at("music", "a/02.flac").unwrap().expect("a/02");
    let b1 = store.track_at("music", "b/01.flac").unwrap().expect("b/01");
    let album_a = store.album_for_track(&a2).unwrap().expect("album a");
    let album_b = store.album_for_track(&b1).unwrap().expect("album b");
    let db = app.db();
    db.execute(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, 'retag_followed', 1234)",
        [&album_b],
    )
    .expect("album b marked");
    let marks = || -> Vec<(String, String, i64)> {
        let mut statement = db
            .prepare(
                "SELECT local_album_id, cause, noticed_at FROM library_album_takeovers \
                 ORDER BY local_album_id",
            )
            .expect("marks read");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("marks")
            .collect::<Result<_, _>>()
            .expect("marks")
    };
    let correct = |preview: String, apply: String, selection: Value| async move {
        let (status, body) = app
            .call("POST", &preview, &headers, Some(selection.clone()))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut request = selection;
        request["preview_token"] = body["preview_token"].clone();
        let (status, body) = app.call("POST", &apply, &headers, Some(request)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };

    correct(
        "/api/v3/library/albums/merge-preview".to_owned(),
        "/api/v3/library/albums/merge".to_owned(),
        json!({"track_ids": [b1], "target_album_id": album_a}),
    )
    .await;
    assert_eq!(
        marks(),
        vec![(album_a.clone(), "retag_followed".to_owned(), 1234)],
        "the mark moved to the album that stays"
    );

    let split = correct(
        format!("/api/v3/library/albums/{album_a}/split-preview"),
        format!("/api/v3/library/albums/{album_a}/split"),
        json!({"track_ids": [a2], "title": "Bonus"}),
    )
    .await;
    let made = split["target_album_id"].as_str().expect("new album");
    let mut expected = vec![
        (album_a.clone(), "retag_followed".to_owned(), 1234),
        (made.to_owned(), "retag_followed".to_owned(), 1234),
    ];
    expected.sort();
    assert_eq!(
        marks(),
        expected,
        "the album made by the split is marked too"
    );
    rig.stop().await;
}

/// "Manage it again" hands a taken-over album back: the files are
/// recorded as they are at that moment and the worker writes them from
/// the edition. The permission covers those bytes only: an edit made
/// while the run could not go ahead is protected like any other, and so
/// is one made after the app wrote the album again.
#[tokio::test]
async fn handing_an_album_back_lets_the_app_write_again() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::tags::{TagField, read_fields};

    let managed = ManagedLibrary::start("lib-takeover-hand-back", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 2).await;
    managed.activate().await;
    managed.look(&album).await;
    let title = |path: &Path| read_fields(path).expect("tags read")[&TagField::Title].clone();
    let hand_back = || {
        Mutations::new(&managed.library)
            .hand_back_management(&album, "admin")
            .expect("hand-back answers")
    };
    assert!(!hand_back(), "an album the app manages has nothing to lift");

    // The second file's match names a track the edition does not have, so
    // no run of the album can go ahead.
    let mapped: String = managed
        .db()
        .query_row(
            "SELECT release_track_mbid FROM local_track_external_identities \
             WHERE local_track_id = ?1",
            [&tracks[1]],
            |row| row.get(0),
        )
        .expect("mapping");
    let map_second = |release_track: &str| {
        managed
            .db()
            .execute(
                "UPDATE local_track_external_identities SET release_track_mbid = ?2, \
                 row_revision = row_revision + 1 WHERE local_track_id = ?1",
                [tracks[1].as_str(), release_track],
            )
            .expect("mapping set");
    };
    map_second("no-such-track");
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));

    assert!(hand_back());
    assert_eq!(managed.mark(&album), None);
    for track in &tracks {
        assert_eq!(
            managed.recorded(track),
            Some(file_sha256(&managed.path(track))),
            "the bytes as they are at the hand-back"
        );
    }
    assert_eq!(
        managed.count(
            "SELECT COUNT(*) FROM library_catalog_actions a JOIN auth_users u \
             ON u.id = a.actor_user_id WHERE a.action_kind = 'management_handed_back' \
             AND u.id = 'admin'"
        ),
        1
    );
    assert!(!hand_back(), "handed back once");

    // The run is blocked. The file is edited again meanwhile, and then the
    // block goes: that later edit was never handed over.
    managed.worker_looks().await;
    assert_eq!(title(&managed.path(&tracks[0])), vec!["By Hand".to_owned()]);
    managed.edit_outside(&tracks[0], title_edit("Again")).await;
    let edited = file_sha256(&managed.path(&tracks[0]));
    map_second(&mapped);
    managed.look(&album).await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(
        file_sha256(&managed.path(&tracks[0])),
        edited,
        "nothing written"
    );

    // Handed back with nothing in the way: the worker writes the edition.
    assert!(hand_back());
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(title(&managed.path(&tracks[0])), vec!["first 1".to_owned()]);

    // And an edit after that takes the album over again.
    managed.edit_outside(&tracks[0], title_edit("Third")).await;
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(title(&managed.path(&tracks[0])), vec!["Third".to_owned()]);
}

/// "Manage it again" spends v2's record on every track, a v2 row a later
/// in-app write carried included: a file that cannot be read then has no
/// record at all, never v2's stale one.
#[tokio::test]
async fn handing_back_spends_a_carried_v2_record() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::record::record;

    let managed = ManagedLibrary::start("lib-takeover-hand-back-v2", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 2).await;
    managed.activate().await;
    managed.look(&album).await;
    let db = managed.db();
    assert_eq!(
        db.execute(
            "UPDATE library_track_management_state SET written_sha256 = 'x', \
             managed_path_revision = 'v2', last_outcome = 'succeeded', \
             last_operation_job_id = NULL WHERE local_track_id = ?1",
            [&tracks[1]],
        )
        .expect("v2 row carried by a v3 write"),
        1
    );
    db.execute(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, 'files_changed', 1000)",
        [&album],
    )
    .expect("album marked");
    std::fs::remove_file(managed.path(&tracks[1])).expect("file gone");

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&album, "admin")
            .expect("hand-back answers")
    );
    assert!(!record(&db, &tracks[1]).expect("record").exists());
    assert_eq!(
        managed.recorded(&tracks[0]),
        Some(file_sha256(&managed.path(&tracks[0])))
    );
}

/// Handing every album back at once only stamps them: no file is read in
/// the request. The worker then hands back one album a step, and a
/// restart in between loses none.
#[tokio::test]
async fn handing_every_album_back_takes_one_step_each() {
    use droppedneedle_library::management::automatic::Settle;
    use droppedneedle_library::management::service::Management;
    use droppedneedle_library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-takeover-hand-back-all").await;
    for folder in ["a", "b", "c"] {
        plant(&music, &format!("{folder}/01.flac"), "management_full.flac");
    }
    add_music_root(&library, &music).await;
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db");
    db.execute_batch(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         SELECT id, 'files_changed', 1000 FROM local_albums; \
         DELETE FROM library_management_automatic_queue;",
    )
    .expect("three albums marked");
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).expect("count") };
    let recorded = "SELECT COUNT(*) FROM library_track_management_state \
                    WHERE written_sha256 IS NOT NULL";

    assert_eq!(
        Mutations::new(&library)
            .hand_back_all("admin")
            .expect("stamped"),
        3
    );
    assert_eq!(
        Mutations::new(&library)
            .hand_back_all("admin")
            .expect("stamped"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM library_album_takeovers WHERE handed_back_at IS NOT NULL"),
        3,
        "still marked, waiting for their turn"
    );
    assert_eq!(count(recorded), 0, "no file was read");
    let waiting = Management::new(&library).takeovers(100).expect("waiting");
    assert_eq!((waiting.total, waiting.handing_back), (0, 3));

    drop(library);
    let library = open_library(&scratch).await;
    let management = Management::new(&library);
    let mut settle = Settle::default();
    for left in [2, 1, 0] {
        assert!(management.automatic_step(&mut settle).expect("worker step"));
        assert_eq!(count("SELECT COUNT(*) FROM library_album_takeovers"), left);
    }
    assert_eq!(count(recorded), 3);
    assert_eq!(
        count("SELECT COUNT(*) FROM library_management_automatic_queue"),
        3,
        "each album is queued for automatic management"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM library_catalog_actions \
             WHERE action_kind = 'management_handed_back' AND actor_user_id = 'admin'"
        ),
        3,
        "said once per album, by the person who asked"
    );
    for folder in ["a", "b", "c"] {
        let sha: String = db
            .query_row(
                "SELECT s.written_sha256 FROM library_track_management_state s \
                 JOIN local_tracks t ON t.id = s.local_track_id WHERE t.relative_path = ?1",
                [format!("{folder}/01.flac")],
                |row| row.get(0),
            )
            .expect("recorded bytes");
        assert_eq!(sha, file_sha256(&music.join(format!("{folder}/01.flac"))));
    }
}

/// What the album page and Library Management read about an album left
/// unmanaged, and the two routes that hand albums back.
#[tokio::test]
async fn left_unmanaged_albums_read_back_and_hand_back() {
    let rig = Rig::start("lib-takeover-reads").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    app.scan_library().await;
    let track = rig
        .scan_store
        .track_at("music", "a/01.flac")
        .unwrap()
        .expect("track");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let db = app.db();
    let mark = || {
        db.execute(
            "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
             VALUES (?1, 'files_changed', 1234)",
            [&album],
        )
        .expect("album marked");
    };
    let album_url = format!("/api/v3/library/albums/{album}");
    let hold = || async {
        let (status, body) = app.call("GET", &album_url, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["id"], json!(album), "the album's own fields stay");
        body["management_hold"].clone()
    };
    assert_eq!(hold().await, Value::Null);

    mark();
    assert_eq!(
        hold().await,
        json!({
            "kind": "files_changed",
            "held_at": 1234,
            "exclusion_revision": null,
            "handing_back": false
        })
    );
    let (status, list) = app
        .call(
            "GET",
            "/api/v3/library/management/takeovers",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], json!(1));
    assert_eq!(list["handing_back"], json!(0));
    assert_eq!(list["items"][0]["album_id"], json!(album));
    assert_eq!(list["items"][0]["cause"], json!("files_changed"));
    assert_eq!(list["items"][0]["noticed_at"], json!(1234));

    // A person's own choice is what the page shows when both are there.
    db.execute(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) SELECT ?1, 'manual', id, 5 FROM auth_users LIMIT 1",
        [&album],
    )
    .expect("album excluded");
    let own = hold().await;
    assert_eq!(own["kind"], json!("your_choice"));
    assert_eq!(own["held_at"], json!(5000));
    assert_eq!(own["exclusion_revision"], json!(1));
    db.execute("DELETE FROM library_management_exclusions", [])
        .expect("exclusion lifted");

    let hand_back = format!("/api/v3/library/albums/{album}/management/hand-back");
    let (status, body) = app.call("POST", &hand_back, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"handed_back": true}));
    let (status, body) = app.call("POST", &hand_back, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"handed_back": false}));
    assert_eq!(hold().await, Value::Null);

    // All at once: the route answers the count and the worker does the
    // rest.
    mark();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/management/hand-back",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"albums": 1}));
    // The app's own worker loop hands it back.
    app.until("the worker hands the album back", || {
        let left: i64 = db
            .query_row("SELECT COUNT(*) FROM library_album_takeovers", [], |row| {
                row.get(0)
            })
            .expect("marks read");
        async move { (left == 0).then_some(()) }
    })
    .await;
    assert_eq!(hold().await, Value::Null);
    rig.stop().await;
}

/// A hand-back that cannot read the album's files changes nothing: the
/// mark stays and nothing is recorded. Handed back all at once, the
/// worker then waits before it tries that album again, instead of
/// trying it on every step.
#[tokio::test]
async fn a_hand_back_that_cannot_read_the_files_keeps_the_mark() {
    use droppedneedle_library::management::automatic::Settle;
    use droppedneedle_library::management::service::Management;
    use droppedneedle_library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-takeover-hand-back-fails").await;
    let file = plant(&music, "a/01.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db");
    db.execute_batch(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         SELECT id, 'files_changed', 1000 FROM local_albums;",
    )
    .expect("album marked");
    let album: String = db
        .query_row("SELECT id FROM local_albums", [], |row| row.get(0))
        .expect("album");
    // The share went away between the scan and the hand-back.
    std::fs::remove_file(&file).expect("file gone");
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).expect("count") };
    let recorded = "SELECT COUNT(*) FROM library_track_management_state \
                    WHERE written_sha256 IS NOT NULL";

    let refused = Mutations::new(&library).hand_back_management(&album, "admin");
    assert!(
        matches!(refused, Err(MutationError::Conflict(_))),
        "{refused:?}"
    );
    assert_eq!(count("SELECT COUNT(*) FROM library_album_takeovers"), 1);
    assert_eq!(count(recorded), 0);

    assert_eq!(
        Mutations::new(&library)
            .hand_back_all("admin")
            .expect("stamped"),
        1
    );
    let management = Management::new(&library);
    let mut settle = Settle::default();
    for _ in 0..3 {
        management.automatic_step(&mut settle).expect("worker step");
    }
    assert_eq!(
        count("SELECT hand_back_tries FROM library_album_takeovers"),
        1,
        "tried once, then it waits"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM library_album_takeovers WHERE handed_back_at IS NOT NULL"),
        1,
        "still waiting for its turn"
    );
    assert_eq!(count(recorded), 0);
}

/// "Manage it again" always leads to a fresh look: no run made before the
/// hand-back answers for the album. Here the run is undone and the album
/// handed back with no scan in between, so nothing about its files has
/// changed in the catalog since the first run.
#[tokio::test]
async fn a_handed_back_album_is_looked_at_afresh() {
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-takeover-afresh", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    managed.activate().await;
    let planted = managed.path(&tracks[0]);
    managed.look(&album).await;
    let placed = managed.path(&tracks[0]);
    assert_ne!(placed, planted);

    let management = managed.management();
    let (history, _) = management.history(Default::default()).expect("history");
    let run = history
        .iter()
        .find(|item| item.origin == "scan_discovered")
        .expect("an automatic run");
    let undo = management
        .undo_preview(&run.job.id, run.job.row_revision, "undo", "admin")
        .expect("undo preview");
    assert!(
        management_step(&managed.library).await,
        "undo planning runs"
    );
    let undone = management_apply(&managed.library, &undo).await;
    assert_eq!(undone.job.state.as_str(), "succeeded");
    assert_eq!(managed.path(&tracks[0]), planted);
    assert_eq!(managed.mark(&album).as_deref(), Some("run_undone"));

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&album, "admin")
            .expect("hand-back answers")
    );
    managed.worker_looks().await;
    assert_eq!(managed.path(&tracks[0]), placed, "the run is done again");
    assert_eq!(managed.mark(&album), None);
}

impl ManagedLibrary {
    /// Retag every one of `tracks` to the album `title` outside the app,
    /// then scan.
    async fn rename_outside(&self, tracks: &[String], title: &str) {
        use droppedneedle_library::tags::{TagEdit, TagField, save_tags};
        for track in tracks {
            save_tags(
                &self.path(track),
                &[TagEdit::verbatim(TagField::Album, vec![title.to_owned()])],
            )
            .expect("retagged outside the app");
        }
        rescan(&self.library).await;
    }

    /// The album the track row is on, whether its file is there or not.
    fn album_of(&self, track: &str) -> String {
        self.db()
            .query_row(
                "SELECT local_album_id FROM local_tracks WHERE id = ?1",
                [track],
                |row| row.get(0),
            )
            .expect("track row")
    }

    /// Whether the album waits for the worker with a scan's stamp that it
    /// was followed across a retag.
    fn stamped(&self, album: &str) -> bool {
        self.db()
            .query_row(
                "SELECT COUNT(*) FROM library_management_automatic_queue \
                 WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL",
                [album],
                |row| row.get::<_, i64>(0),
            )
            .expect("stamp reads")
            == 1
    }
}

/// The album tag in the file.
fn album_tag(path: &Path) -> Vec<String> {
    use droppedneedle_library::tags::{TagField, read_fields};
    read_fields(path).expect("tags read")[&TagField::Album].clone()
}

const STEPPED_BACK: &str =
    "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'management_stepped_back'";

/// The app's own write regroups an album too: the edition's title is not
/// the album tag in the files, so the run changes the album tag and the
/// next scan files them under a new name. With seven of the album's twelve
/// tracks gone for more than a day, that scan follows the album, and
/// nothing marks it.
#[tokio::test]
async fn the_apps_own_regroup_of_a_mostly_missing_album_is_followed_unmarked() {
    let managed = ManagedLibrary::start("lib-takeover-own-regroup", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 12).await;
    managed
        .db()
        .execute(
            "INSERT INTO library_management_metadata_snapshots (id, provider, entity_kind, \
             entity_id, input_hash, canonical_payload_json, payload_sha256, fetched_at) \
             SELECT id || '-retitled', provider, entity_kind, entity_id, input_hash, \
             json_set(canonical_payload_json, '$.title', 'second (remastered)'), \
             lower(hex(randomblob(32))), fetched_at + 1 \
             FROM library_management_metadata_snapshots \
             WHERE json_extract(canonical_payload_json, '$.title') = 'second'",
            [],
        )
        .expect("edition retitled");
    managed.activate().await;
    for track in &tracks[5..] {
        std::fs::remove_file(managed.path(track)).expect("file removed");
    }
    rescan(&managed.library).await;
    managed
        .db()
        .execute(
            "UPDATE local_tracks SET missing_since = missing_since - 86401 \
             WHERE availability = 'missing'",
            [],
        )
        .expect("gone a day");
    let key = || -> String {
        managed
            .db()
            .query_row(
                "SELECT grouping_key FROM local_albums WHERE id = ?1",
                [&album],
                |row| row.get(0),
            )
            .expect("album key")
    };
    let before = key();

    managed.look(&album).await;
    for track in &tracks[..5] {
        assert_eq!(
            managed.recorded(track),
            Some(file_sha256(&managed.path(track))),
            "the app wrote the file"
        );
        assert_eq!(album_tag(&managed.path(track)), vec!["second (remastered)"]);
    }
    rescan(&managed.library).await;
    managed.worker_looks().await;

    assert_ne!(key(), before, "the scan regrouped the files");
    for track in &tracks {
        assert_eq!(managed.album_of(track), album, "followed");
    }
    assert_eq!(managed.count("SELECT COUNT(*) FROM local_albums"), 2);
    assert_eq!(managed.count("SELECT COUNT(*) FROM library_retag_pairs"), 0);
    assert_eq!(
        managed.mark(&album),
        None,
        "the app's own write never marks"
    );
    assert_eq!(managed.count(STEPPED_BACK), 0);
}

/// An album whose edition a person chose, and whose files the app has
/// never written, retagged by hand to a new name in a folder managed
/// automatically: the scan follows it, and the worker leaves it unmanaged
/// (`retag_followed`) without writing anything, said once. Handed back,
/// the worker writes the edition and does not mark it again.
#[tokio::test]
async fn a_hand_renamed_album_with_a_chosen_edition_is_left_unmanaged() {
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-takeover-renamed", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 2).await;
    managed.activate().await;
    managed.rename_outside(&tracks, "Renamed").await;
    let bytes = || -> Vec<String> {
        tracks
            .iter()
            .map(|track| file_sha256(&managed.path(track)))
            .collect()
    };
    let edited = bytes();
    assert_eq!(managed.album_of(&tracks[0]), album, "followed");
    assert!(managed.stamped(&album));

    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    assert!(!managed.stamped(&album), "one follow marks once");
    assert_eq!(managed.count(STEPPED_BACK), 1);
    assert_eq!(bytes(), edited, "nothing written");
    assert_eq!(album_tag(&managed.path(&tracks[0])), vec!["Renamed"]);
    let skipped = managed.management().automatic_skipped(10).expect("skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reason.code, "TAKEN_OVER");

    // A scan of unchanged files and another look change nothing.
    rescan(&managed.library).await;
    managed.look(&album).await;
    assert_eq!(managed.count(STEPPED_BACK), 1);
    assert_eq!(bytes(), edited);

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&album, "admin")
            .expect("hand-back answers")
    );
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(
        album_tag(&managed.path(&tracks[0])),
        vec!["second"],
        "the edition's tags written"
    );

    // The app's write regroups the files again: no stamp, no mark.
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert_eq!(managed.album_of(&tracks[0]), album);
    assert_eq!(managed.mark(&album), None, "not marked again");
    assert_eq!(managed.count(STEPPED_BACK), 1);
}

/// The same hand rename in a folder without automatic management marks
/// nothing: the worker drops the album, stamp and all. And in a folder
/// with it, a rename made in the app between the scan and the worker's
/// look does not cancel the stamp: the album is still left unmanaged and
/// its files keep the hand retag's tags.
#[tokio::test]
async fn a_hand_renamed_album_is_marked_only_where_the_folder_is_managed() {
    use droppedneedle_library::publish::planner::PlanKind;

    let manual = ManagedLibrary::start("lib-takeover-renamed-manual", |_| {}).await;
    manual.add_album("first", 1).await;
    let (album, tracks) = manual.add_album("second", 2).await;
    manual.worker_looks().await;
    manual.rename_outside(&tracks, "Renamed").await;
    assert_eq!(manual.album_of(&tracks[0]), album, "followed");
    assert!(
        manual.stamped(&album),
        "the scan cannot tell the folder apart"
    );
    manual.worker_looks().await;
    assert_eq!(manual.mark(&album), None);
    assert_eq!(
        manual.count("SELECT COUNT(*) FROM library_management_automatic_queue"),
        0,
        "the queue row is gone with its stamp"
    );

    let managed = ManagedLibrary::start("lib-takeover-renamed-moved", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 2).await;
    managed.activate().await;
    managed.rename_outside(&tracks, "Renamed").await;
    assert!(managed.stamped(&album));
    let relative: String = managed
        .db()
        .query_row(
            "SELECT relative_path FROM local_tracks WHERE id = ?1",
            [&tracks[0]],
            |row| row.get(0),
        )
        .expect("track path");
    publish_one(
        &managed.library,
        PlanKind::Move,
        &album,
        &relative,
        Some("moved/01.flac"),
        &[],
    );
    assert!(managed.path(&tracks[0]).ends_with("moved/01.flac"));
    assert!(
        managed.recorded(&tracks[0]).is_some(),
        "the rename recorded the hand-retagged bytes as the app's"
    );

    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    for track in &tracks {
        assert_eq!(album_tag(&managed.path(track)), vec!["Renamed"]);
    }
}

impl ManagedLibrary {
    /// The track's path inside the music folder.
    fn relative(&self, track: &str) -> String {
        self.db()
            .query_row(
                "SELECT relative_path FROM local_tracks WHERE id = ?1",
                [track],
                |row| row.get(0),
            )
            .expect("track path")
    }

    /// Rename the track's file in the app, as the album page does: a move
    /// with no tag values.
    fn rename_in_app(&self, album: &str, track: &str, dest_rel: &str) {
        publish_one(
            &self.library,
            droppedneedle_library::publish::planner::PlanKind::Move,
            album,
            &self.relative(track),
            Some(dest_rel),
            &[],
        );
        assert!(self.path(track).ends_with(dest_rel));
    }

    /// Change the folder's rules for when it manages albums by itself.
    fn set_automatic(&self, scan_discovered: bool, acquisitions: bool) {
        use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
        let mut settings = self
            .library
            .settings
            .store()
            .get::<LibraryManagement>()
            .expect("settings read");
        for assignment in settings
            .root_assignments
            .iter_mut()
            .filter(|assignment| assignment.root_id == "music")
        {
            assignment.automatic_scan_discovered = scan_discovered;
            assignment.automatic_acquisitions = acquisitions;
        }
        self.library
            .settings
            .store()
            .save_blocking(settings)
            .expect("settings saved");
    }

    /// A person's preview with the folder's profile, planned and applied.
    async fn apply(
        &self,
        kind: droppedneedle_library::management::models::SelectionKind,
        ids: &[&str],
        key: &str,
    ) -> String {
        use droppedneedle_library::management::models::Selection;
        use droppedneedle_library::management::service::PreviewRequest;
        management_run(
            &self.library,
            PreviewRequest {
                selection: Selection {
                    kind,
                    ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                    catalog_filter: None,
                },
                profile_id:
                    droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID
                        .to_owned(),
                expected_settings_revision: self.settings_revision(),
                expected_policy_revision: self
                    .library
                    .core
                    .policy_revision()
                    .expect("policy revision"),
                idempotency_key: Some(key.to_owned()),
                target_root_id: None,
                overrides: None,
            },
        )
        .await
    }
}

/// A person's rename or tag edit in the app over a file changed outside
/// does not take that change over: the app keeps its old record, so the
/// worker's next look leaves the album unmanaged and writes nothing. A
/// preview made before the change is refused when applied after it.
#[tokio::test]
async fn a_persons_write_over_a_hand_edit_leaves_it_the_users() {
    use droppedneedle_library::manage::PreviewItemInput;
    use droppedneedle_library::publish::planner::PlanKind;
    use droppedneedle_library::tags::{TagField, read_fields};

    let managed = ManagedLibrary::start("lib-takeover-own-write", |_| {}).await;
    let (renamed, renamed_tracks) = managed.add_album("first", 1).await;
    let (edited, edited_tracks) = managed.add_album("second", 1).await;
    let (stale, stale_tracks) = managed.add_album("third", 1).await;
    managed.activate().await;
    for album in [&renamed, &edited, &stale] {
        managed.look(album).await;
    }
    let title = |path: &Path| read_fields(path).expect("tags read")[&TagField::Title].clone();

    // A managed tag changed by hand, then the file renamed in the app.
    let track = &renamed_tracks[0];
    managed.edit_outside(track, title_edit("By Hand")).await;
    managed.rename_in_app(&renamed, track, "moved/01.flac");
    let file = managed.path(track);
    let bytes = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&renamed).as_deref(), Some("files_changed"));
    assert_eq!(managed.path(track), file);
    assert_eq!(file_sha256(&file), bytes, "nothing written");
    assert_eq!(title(&file), vec!["By Hand".to_owned()]);

    // A tag edit of another field made in the app writes the edition's
    // whole tag set, so it puts the hand-edited title back itself, as its
    // preview showed. The app keeps its old record all the same, and the
    // worker's look then marks nothing: no managed tag differs any more.
    let track = &edited_tracks[0];
    managed.edit_outside(track, title_edit("By Hand")).await;
    let recorded = managed.recorded(track);
    publish_one(
        &managed.library,
        PlanKind::SamePath,
        &edited,
        &managed.relative(track),
        None,
        &[("date", "2001")],
    );
    assert_eq!(managed.recorded(track), recorded, "the old record kept");
    assert_eq!(title(&managed.path(track)), vec!["second 1".to_owned()]);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&edited), None);
    assert_eq!(
        managed.recorded(track),
        Some(file_sha256(&managed.path(track))),
        "the worker's look took the bytes as the app's"
    );

    // A preview sealed before the hand edit.
    let track = &stale_tracks[0];
    let sealed = managed
        .library
        .publishing
        .plan_preview(
            PlanKind::Move,
            &stale,
            vec![PreviewItemInput {
                root_id: "music".to_owned(),
                rel_path: managed.relative(track),
                dest_rel: Some("moved/03.flac".to_owned()),
                managed_updates: Default::default(),
            }],
        )
        .expect("preview seals");
    managed.edit_outside(track, title_edit("By Hand")).await;
    let file = managed.path(track);
    let bytes = file_sha256(&file);
    assert!(
        managed
            .library
            .publishing
            .apply_preview(&sealed.token, None)
            .is_err()
    );
    assert_eq!(managed.path(track), file);
    assert_eq!(file_sha256(&file), bytes, "the file untouched");
}

/// A person's run of the whole profile over a hand edit writes every
/// managed field from the edition, so it records what it placed: a later
/// change of edition is the app's to write, and nothing marks the album.
#[tokio::test]
async fn a_persons_profile_run_takes_the_files_back() {
    use droppedneedle_library::management::models::SelectionKind;

    let managed = ManagedLibrary::start("lib-takeover-own-apply", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    managed.activate().await;
    managed.look(&album).await;
    let track = &tracks[0];
    managed.edit_outside(track, title_edit("By Hand")).await;
    managed.apply(SelectionKind::Albums, &[&album], "own").await;
    let file = managed.path(track);
    assert_eq!(managed.recorded(track), Some(file_sha256(&file)));

    let chosen: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    let other = if chosen == REL_1 { REL_2 } else { REL_1 };
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, other, Some("admin"))
        .await
        .expect("edition chosen");
    managed.look(&album).await;
    assert_eq!(managed.mark(&album), None);
    let last = managed.automatic_runs().pop().expect("a run");
    assert_eq!(last.1.as_deref(), Some("MANAGEMENT_APPLIED"), "it wrote");
}

/// A change only to what the app does not manage stays accepted through a
/// person's rename in the app and through the app's own runs: no mark, and
/// the record follows the bytes.
#[tokio::test]
async fn an_unmanaged_change_stays_accepted_through_every_write() {
    use droppedneedle_library::tags::save::read_document;
    use droppedneedle_library::tags::{TagEdit, save_tags};

    let managed = ManagedLibrary::start("lib-takeover-unmanaged-write", |_| {}).await;
    let (moved, moved_tracks) = managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 1).await;
    managed.activate().await;
    managed.look(&album).await;
    managed.look(&moved).await;
    let calm = || TagEdit::native("vorbis:MOOD", vec!["Calm".to_owned()]);
    let mood = |path: &Path| {
        read_document(path)
            .expect("tags read")
            .native
            .get("vorbis:MOOD")
            .cloned()
    };

    // Changed by hand where the profile does not reach, then renamed in
    // the app.
    let track = &tracks[0];
    managed.edit_outside(track, calm()).await;
    managed.rename_in_app(&album, track, "moved/01.flac");
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album), None);
    let file = managed.path(track);
    assert_eq!(
        managed.recorded(track),
        Some(file_sha256(&file)),
        "the bytes as found"
    );
    assert_eq!(mood(&file), Some(vec!["Calm".to_owned()]));

    // What the app wants changes: the worker writes, and marks nothing.
    let chosen: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    // The album's two editions differ only in their second id group.
    let other = if chosen.contains("-0000-") {
        chosen.replacen("-0000-", "-0001-", 1)
    } else {
        chosen.replacen("-0001-", "-0000-", 1)
    };
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, &other, Some("admin"))
        .await
        .expect("edition chosen");
    managed.look(&album).await;
    let last = managed.automatic_runs().pop().expect("a run");
    assert_eq!(last.0, "succeeded");
    assert_ne!(last.1.as_deref(), Some("MANAGEMENT_UNCHANGED"), "it wrote");
    assert_eq!(managed.mark(&album), None);
    let file = managed.path(track);
    assert_eq!(managed.recorded(track), Some(file_sha256(&file)));
    assert_eq!(mood(&file), Some(vec!["Calm".to_owned()]));

    // Moved by hand and changed where the profile does not reach: the
    // worker puts the file back and records the bytes it placed.
    let track = &moved_tracks[0];
    let placed = managed.path(track);
    save_tags(&placed, &[calm()]).expect("changed outside the app");
    let elsewhere = managed.music.join("elsewhere/moved.flac");
    std::fs::create_dir_all(elsewhere.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&placed, &elsewhere).expect("moved by hand");
    rescan(&managed.library).await;
    assert_eq!(managed.path(track), elsewhere, "the track follows the file");
    managed.look(&moved).await;
    assert_eq!(managed.path(track), placed, "put back by the naming rules");
    assert_eq!(managed.mark(&moved), None);
    assert_eq!(managed.recorded(track), Some(file_sha256(&placed)));
    assert_eq!(mood(&placed), Some(vec!["Calm".to_owned()]));
}

/// A file retagged and renamed together comes back as a new track row
/// with no record, beside the row of the file the app wrote, and is placed
/// on the old row's track by its ids at that scan's end, with the old
/// row's history. It still counts as changed since the app wrote it: the
/// album is left unmanaged and nothing is written. Files that only went
/// away mark nothing.
#[tokio::test]
async fn a_file_retagged_and_renamed_together_is_still_the_users() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::tags::{TagEdit, TagField, save_tags};

    let managed = ManagedLibrary::start("lib-takeover-retag-rename", |_| {}).await;
    let (whole, whole_tracks) = managed.add_album("first", 2).await;
    let (one, one_tracks) = managed.add_album("second", 2).await;
    let (pruned, pruned_tracks) = managed.add_album("third", 2).await;
    managed.activate().await;
    for album in [&whole, &one, &pruned] {
        managed.look(album).await;
    }
    // The scan after the app's own write files each album under the
    // release it wrote.
    rescan(&managed.library).await;
    managed.worker_looks().await;
    let retag_and_rename = |track: &str, edit: TagEdit, to: &str| -> PathBuf {
        let from = managed.path(track);
        save_tags(&from, &[edit]).expect("retagged outside the app");
        let to = managed.music.join(to);
        std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&from, &to).expect("renamed by hand");
        to
    };

    let old_paths: Vec<PathBuf> = whole_tracks.iter().map(|t| managed.path(t)).collect();
    let edition_tag = album_tag(&old_paths[0]);
    managed
        .db()
        .execute(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('admin', 'track', ?1, 1)",
            [&whole_tracks[0]],
        )
        .expect("track starred");
    // Every file of the album: its release kept, the album retitled.
    let files: Vec<PathBuf> = whole_tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            retag_and_rename(
                track,
                TagEdit::verbatim(TagField::Album, vec!["Retagged".to_owned()]),
                &format!("retagged/{:02}.flac", index + 1),
            )
        })
        .collect();
    // One file of another album retitled and renamed, the rest untouched.
    let retitled = retag_and_rename(&one_tracks[0], title_edit("By Hand"), "second/by-hand.flac");
    // One file of a third album deleted, none added.
    std::fs::remove_file(managed.path(&pruned_tracks[1])).expect("file removed");
    rescan(&managed.library).await;
    while managed.library.identification.identify_tick().await > 0 {}
    let new_row = |file: &Path| -> String {
        let relative = file
            .strip_prefix(&managed.music)
            .expect("under the root")
            .to_string_lossy()
            .into_owned();
        managed
            .library
            .scan_store
            .track_at("music", &relative)
            .unwrap()
            .expect("track indexed")
    };
    let db = managed.db();
    for (old, file) in whole_tracks.iter().zip(&files) {
        let track = new_row(file);
        assert!(!whole_tracks.contains(&track), "a new track row");
        assert_eq!(managed.album_of(&track), whole, "beside the old rows");
        assert_eq!(managed.recorded(&track), None);
        let on = placement(&db, &track).1;
        assert!(on.is_some(), "placed at the scan's end");
        assert_eq!(on, placement(&db, old).1, "on the old row's track");
    }
    assert_eq!(managed.mark(&whole), None, "the check of the bytes tells");
    assert_eq!(holdings(&db, &new_row(&files[0]))[1], 1, "the star follows");
    assert_eq!(managed.album_of(&new_row(&retitled)), one);
    let bytes =
        |paths: &[PathBuf]| -> Vec<String> { paths.iter().map(|p| file_sha256(p)).collect() };
    let whole_bytes = bytes(&files);
    let one_bytes = bytes(std::slice::from_ref(&retitled));

    // The worker counts the placed rows as changed and steps back.
    for album in [&whole, &one, &pruned] {
        managed.look(album).await;
    }
    assert_eq!(managed.mark(&whole).as_deref(), Some("files_changed"));
    assert_eq!(bytes(&files), whole_bytes, "nothing written");
    assert_eq!(album_tag(&files[0]), vec!["Retagged"]);
    assert_eq!(managed.mark(&one).as_deref(), Some("files_changed"));
    assert_eq!(bytes(std::slice::from_ref(&retitled)), one_bytes);
    assert_eq!(
        managed.mark(&pruned),
        None,
        "files that went away mark nothing"
    );

    let missing_since = |track: &str| -> (String, Option<f64>) {
        managed
            .db()
            .query_row(
                "SELECT availability, missing_since FROM local_tracks WHERE id = ?1",
                [track],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("old row")
    };
    let gone: Vec<(String, Option<f64>)> = whole_tracks.iter().map(|t| missing_since(t)).collect();
    assert!(
        gone.iter()
            .all(|(state, since)| state == "missing" && since.is_some())
    );
    managed
        .db()
        .execute(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('admin', 'album', ?1, 1)",
            [&whole],
        )
        .expect("album favorited");

    // Handed back: the rows left behind lose their record with it, so the
    // album is not marked again. The naming rules lead the files back to
    // the paths their old rows hold: those rows give the paths up and
    // stay missing under their own ids.
    let runs = managed.automatic_runs().len();
    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&whole, "admin")
            .expect("hand-back answers")
    );
    managed.worker_looks().await;
    assert_eq!(managed.mark(&whole), None);
    let after = managed.automatic_runs();
    assert!(after.len() > runs, "the hand-back runs");
    for run in &after[runs..] {
        assert_eq!(run.0, "succeeded", "{after:?}");
        assert_ne!(run.1.as_deref(), Some("MANAGEMENT_PARTIAL"), "{after:?}");
    }
    for ((old, path), before) in whole_tracks.iter().zip(&old_paths).zip(&gone) {
        let new = new_row(path);
        assert_ne!(&new, old, "the old row is off its path");
        assert_eq!(managed.album_of(&new), whole);
        assert_eq!(album_tag(path), edition_tag);
        assert_eq!(
            &missing_since(old),
            before,
            "still missing, since the same day"
        );
        assert_ne!(&managed.path(old), path);
    }
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_kind = 'album' \
             AND item_id = '{whole}'"
        )),
        1,
        "the album keeps its id and its favourite"
    );
    rescan(&managed.library).await;
    managed.look(&whole).await;
    assert_eq!(managed.mark(&whole), None, "not marked again");
}

/// An import that brings back a track the app wrote and that went missing
/// marks nothing: the app wrote the new file too.
#[tokio::test]
async fn an_import_in_place_of_a_missing_track_marks_nothing() {
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};

    let managed = ManagedLibrary::start("lib-takeover-import-missing", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 2).await;
    managed.activate().await;
    managed.set_automatic(true, true);
    managed.look(&album).await;
    rescan(&managed.library).await;
    managed.worker_looks().await;
    std::fs::remove_file(managed.path(&tracks[1])).expect("file removed");
    rescan(&managed.library).await;

    let release_mbid: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    let release = managed
        .library
        .identify_store
        .release(&release_mbid, None)
        .unwrap()
        .expect("release stored");
    let source = managed.scratch.to_path_buf().join("incoming/02.flac");
    std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixtures_dir().join("management_full.flac"), &source).expect("download planted");
    let imported = managed
        .library
        .imports
        .import_download(&DownloadImport {
            task_id: "task-1".to_owned(),
            origin: ImportOrigin::Download,
            staging: None,
            release,
            files: vec![ImportSource {
                path: source,
                track: 1,
                replaces: Vec::new(),
            }],
            extras: Vec::new(),
            actor: None,
        })
        .expect("imported");
    rescan(&managed.library).await;
    managed.look(&album).await;
    managed.look(&imported.album_id).await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(managed.mark(&imported.album_id), None);
    assert_eq!(managed.count(STEPPED_BACK), 0);
}

/// An import onto an album whose match cannot be read writes none of the
/// album's identity: the person's album row, the track rows and the flag
/// stay as they were, the file still lands, and the album is offered to
/// identification instead.
#[tokio::test]
async fn an_import_that_cannot_read_the_albums_match_leaves_it_alone() {
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};

    let (scratch, library, _, at_rest, _) = album_at_rest("lib-import-unreadable-match", 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &at_rest));
    while library.identification.identify_tick().await > 0 {}
    let payload: String = db
        .query_row(
            "SELECT canonical_payload_json FROM library_management_metadata_snapshots \
             WHERE entity_kind = 'release' AND entity_id = ?1 \
             ORDER BY fetched_at DESC LIMIT 1",
            [REL_1],
            |row| row.get(0),
        )
        .expect("release stored");
    let release: Release = serde_json::from_str(&payload).expect("release decodes");
    let import = |task: &str, tracks: &[usize]| {
        let files = tracks
            .iter()
            .map(|&track| {
                let source = scratch
                    .to_path_buf()
                    .join(format!("incoming/{task}/{track}.flac"));
                std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
                std::fs::copy(fixtures_dir().join("flac_full_02.flac"), &source)
                    .expect("download planted");
                ImportSource {
                    path: source,
                    track,
                    replaces: Vec::new(),
                }
            })
            .collect();
        library
            .imports
            .import_download(&DownloadImport {
                task_id: task.to_owned(),
                origin: ImportOrigin::Download,
                staging: None,
                release: release.clone(),
                files,
                extras: Vec::new(),
                actor: None,
            })
            .expect("imported")
    };
    let relative = |path: &Path| {
        path.strip_prefix(scratch.to_path_buf().join("music"))
            .expect("under the root")
            .to_string_lossy()
            .into_owned()
    };
    let first = import("task-1", &[0, 1, 2]);
    let album = first.album_id.clone();
    let missing = indexed_at(&library, &relative(&first.paths[1]));
    std::fs::remove_file(&first.paths[1]).expect("file deleted");
    rescan(&library).await;

    // A person's match holding a value the reader cannot decode (the
    // table is not STRICT), and an unsure flag on the album.
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET decision_source = 'manual', \
         release_group_mbid = X'00' WHERE local_album_id = '{album}'; \
         INSERT OR REPLACE INTO library_album_match_state (local_album_id, state, \
         reason_code, release_mbid, candidates_json, updated_at) \
         VALUES ('{album}', 'unconfirmed', 'EDITION_UNCERTAIN', '{REL_1}', '[]', 1);"
    ))
    .expect("match made unreadable");
    let identity = |db: &rusqlite::Connection| -> Vec<String> {
        db.prepare(
            "SELECT 'album ' || quote(release_group_mbid) || ' ' || quote(release_mbid) || ' ' \
             || decision_source || ' ' || row_revision || ' ' || selected_at \
             FROM local_album_external_identities WHERE local_album_id = ?1 \
             UNION ALL SELECT 'track ' || local_track_id || ' ' || recording_mbid || ' ' \
             || quote(release_track_mbid) || ' ' || decision_source || ' ' || row_revision \
             || ' ' || selected_at FROM local_track_external_identities \
             WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1) \
             UNION ALL SELECT 'flag ' || state || ' ' || reason_code \
             FROM library_album_match_state WHERE local_album_id = ?1 ORDER BY 1",
        )
        .expect("prepare")
        .query_map([album.as_str()], |row| row.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    };
    let before = identity(&db);
    assert_eq!(
        before.len(),
        5,
        "album, three tracks and the flag: {before:?}"
    );

    let again = import("task-2", &[1]);
    assert_eq!(again.album_id, album, "the download joins the album");
    assert!(again.paths[0].is_file(), "the file is in place");
    assert_eq!(
        indexed_at(&library, &relative(&again.paths[0])),
        missing,
        "it takes the missing track's id"
    );

    assert_eq!(identity(&db), before, "nothing of the match is written");
    let live_jobs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_identify_jobs WHERE local_album_id = ?1 \
             AND state IN ('queued', 'deferred', 'running')",
            [&album],
            |row| row.get(0),
        )
        .expect("jobs");
    assert!(live_jobs > 0, "the album is offered to identification");
}

/// An imported file and a management preview with the same naming script
/// give the file the same path, so Library Management never moves what an
/// import placed: for the default import script, Picard's single-disc
/// script and an `{initial}` one, with a title long enough to be cut.
#[tokio::test]
async fn import_and_management_name_a_file_alike() {
    use droppedneedle_kernel::runtime_config::secret_sections::{
        DEFAULT_NAMING_SCRIPT, TypedLibrary,
    };
    use droppedneedle_kernel::runtime_config::sections::NamingScript;
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};
    use droppedneedle_library::management::models::{ItemFilter, Selection, SelectionKind};
    use droppedneedle_library::management::service::{Management, PreviewRequest};
    use droppedneedle_library::management::settings as lm;
    use droppedneedle_library::matching::model::ReleaseMedium;

    let scripts = [
        DEFAULT_NAMING_SCRIPT.to_owned(),
        lm::PICARD_STANDARD_NAMING_SOURCE.to_owned(),
        lm::legacy_template_to_script(
            "{initial}/{albumartist}/[{year}] {album}/{disc:02d}-{track:02d} {title}.{ext}",
        )
        .script,
    ];
    let (scratch, library, music) = bare_library("lib-import-naming-alike").await;
    std::fs::create_dir_all(&music).expect("music folder");
    add_music_root(&library, &music).await;
    library
        .scan_store
        .db()
        .run_sql(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    let mut settings = lm::initial_settings("");
    let picard = settings
        .profiles
        .iter()
        .find(|profile| profile.id == lm::PICARD_ORGANIZER_PROFILE_ID)
        .expect("picard preset")
        .clone();
    for (n, script) in scripts.iter().enumerate() {
        settings.naming_scripts.push(NamingScript {
            id: format!("script-{n}"),
            name: format!("Script {n}"),
            source: script.clone(),
            ..NamingScript::default()
        });
        let mut profile = picard.clone();
        profile.id = format!("profile-{n}");
        profile.name = format!("Profile {n}");
        profile.preset_origin = None;
        profile.organization.naming_script_id = format!("script-{n}");
        profile.organization.multi_disc_naming_script_id = None;
        settings.profiles.push(profile);
    }
    let settings = library
        .settings
        .store()
        .save_blocking(settings)
        .expect("settings saved");
    let management = Management::new(&library);

    for (n, script) in scripts.iter().enumerate() {
        let script = script.clone();
        library
            .settings
            .store()
            .update_secret_blocking::<TypedLibrary, String, _>(move |stored| {
                let mut library = stored.into_inner();
                library.naming_script = script;
                Ok(library)
            })
            .expect("import script saved");
        let artist = CreditedArtist {
            id: format!("{n}0000000-0000-4000-8000-00000000000a"),
            name: "The Golden Band".to_owned(),
            sort_name: Some("Golden Band, The".to_owned()),
            join: String::new(),
        };
        let track = ReleaseTrack {
            id: format!("{n}0000000-0000-4000-8000-00000000000b"),
            recording_id: format!("{n}0000000-0000-4000-8000-00000000000c"),
            title: format!("Song {n} {}", "x".repeat(240)),
            disc: 1,
            position: 1,
            absolute_position: 1,
            ..ReleaseTrack::default()
        };
        let release = Release {
            id: format!("{n}0000000-0000-4000-8000-00000000000d"),
            release_group_id: format!("{n}0000000-0000-4000-8000-00000000000e"),
            title: format!("Golden {n}"),
            artists: vec![artist],
            date: Some("2001-02-03".to_owned()),
            original_date: Some("1999-01-01".to_owned()),
            media: vec![ReleaseMedium {
                position: 1,
                format: Some("CD".to_owned()),
                track_count: 1,
                ..ReleaseMedium::default()
            }],
            tracks: vec![track],
            ..Release::default()
        };
        let source = scratch.to_path_buf().join(format!("incoming/{n}/01.flac"));
        std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
        std::fs::copy(fixtures_dir().join("flac_full_02.flac"), &source).expect("download planted");
        let imported = library
            .imports
            .import_download(&DownloadImport {
                task_id: format!("task-{n}"),
                origin: ImportOrigin::Download,
                staging: None,
                release,
                files: vec![ImportSource {
                    path: source,
                    track: 0,
                    replaces: Vec::new(),
                }],
                extras: Vec::new(),
                actor: None,
            })
            .expect("imported");
        let placed = imported.paths[0]
            .strip_prefix(&music)
            .expect("under the root")
            .to_string_lossy()
            .into_owned();

        let handle = management
            .create_preview(
                PreviewRequest {
                    selection: Selection {
                        kind: SelectionKind::Albums,
                        ids: vec![imported.album_id.clone()],
                        catalog_filter: None,
                    },
                    profile_id: format!("profile-{n}"),
                    expected_settings_revision: lm::settings_revision(&settings),
                    expected_policy_revision: library
                        .core
                        .policy_revision()
                        .expect("policy revision"),
                    idempotency_key: Some(format!("alike-{n}")),
                    target_root_id: None,
                    overrides: None,
                },
                "admin",
            )
            .expect("preview queued");
        assert!(management_step(&library).await, "planning runs");
        let page = management
            .items(&handle.job_id, -1, 100, &ItemFilter::default())
            .expect("items page");
        let item = page
            .items
            .iter()
            .find(|item| item.source_rel == placed)
            .expect("the imported file is planned");
        assert_eq!(
            item.dest_rel.as_deref(),
            Some(placed.as_str()),
            "{:?}",
            item.facts.reason
        );
    }
}

/// A file v2 tagged, changed by hand since, then renamed in the app: the
/// rename keeps v2's record, so the worker still leaves the album to the
/// user.
#[tokio::test]
async fn a_rename_in_the_app_keeps_v2s_record() {
    use droppedneedle_library::publish::record::v2_path_revision;

    let managed = ManagedLibrary::start("lib-takeover-v2-renamed", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    managed.activate().await;
    let track = &tracks[0];
    managed
        .db()
        .execute(
            "INSERT INTO library_track_management_state (local_track_id, managed_root_id, \
             managed_path_revision, last_outcome, last_managed_at) \
             VALUES (?1, 'music', ?2, 'succeeded', 1)",
            rusqlite::params![
                track,
                v2_path_revision(
                    "music",
                    &managed.relative(track),
                    &file_sha256(&managed.path(track))
                ),
            ],
        )
        .expect("v2 row");
    managed.edit_outside(track, title_edit("By Hand")).await;
    managed.rename_in_app(&album, track, "moved/01.flac");
    let file = managed.path(track);
    let bytes = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(file_sha256(&file), bytes, "nothing written");
}

/// A preview of two albums where one has a hand edit the worker has not
/// looked at yet: that album's files are blocked and, in a folder managed
/// automatically, the album is left unmanaged; the other album plans. The
/// album previewed by itself plans.
#[tokio::test]
async fn a_wide_preview_never_writes_over_an_unnoticed_hand_edit() {
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};

    let managed = ManagedLibrary::start("lib-takeover-wide-unnoticed", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    let (other, _) = managed.add_album("second", 1).await;
    managed.activate().await;
    managed.look(&album).await;
    managed.look(&other).await;
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;
    let file = managed.path(&tracks[0]);
    let bytes = file_sha256(&file);

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album, &other], "both")
        .await;
    assert_eq!(items.len(), 2);
    for item in &items {
        if item.album_id == album {
            assert_eq!(item.eligibility, Eligibility::Blocked);
            assert_eq!(item.reason().expect("a reason").code, "CHANGED_OUTSIDE");
        } else {
            assert!(item.eligibility.writes(), "{:?}", item.facts);
        }
    }
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "own")
        .await;
    assert!(items[0].eligibility.writes(), "{:?}", items[0].facts);
    assert!(items[0].diff.tags_changed);
    assert_eq!(file_sha256(&file), bytes);
}

impl ManagedLibrary {
    /// A person's restore-originals preview of whole albums, planned.
    async fn restore_preview(
        &self,
        ids: &[&str],
        key: &str,
    ) -> Vec<droppedneedle_library::management::models::PlannedItem> {
        use droppedneedle_library::management::models::{Selection, SelectionKind};
        use droppedneedle_library::management::restore::RestoreRequest;
        let management = self.management();
        let handle = management
            .create_restore_preview(
                RestoreRequest {
                    selection: Selection {
                        kind: SelectionKind::Albums,
                        ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                        catalog_filter: None,
                    },
                    expected_settings_revision: self.settings_revision(),
                    expected_policy_revision: self
                        .library
                        .core
                        .policy_revision()
                        .expect("policy revision"),
                    idempotency_key: key.to_owned(),
                },
                "admin",
            )
            .expect("restore preview queued");
        assert!(management_step(&self.library).await, "planning runs");
        management
            .items(&handle.job_id, -1, 100, &Default::default())
            .expect("items")
            .items
    }
}

/// In a folder managed by hand, a wide preview blocks an album with an
/// unnoticed hand edit and leaves it unmarked. A person's run of the
/// album by itself takes the files back, so after a change of edition a
/// wide preview plans it again.
#[tokio::test]
async fn a_wide_preview_in_a_manual_folder_blocks_without_marking() {
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};

    let managed = ManagedLibrary::start("lib-takeover-wide-manual", |_| {}).await;
    let (other, _) = managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 1).await;
    managed.worker_looks().await;
    managed
        .apply(SelectionKind::Albums, &[&album], "album")
        .await;
    managed
        .apply(SelectionKind::Albums, &[&other], "other")
        .await;
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album, &other], "both")
        .await;
    for item in items.iter().filter(|item| item.album_id == album) {
        assert_eq!(item.eligibility, Eligibility::Blocked);
        assert_eq!(item.reason().expect("a reason").code, "CHANGED_OUTSIDE");
    }
    assert_eq!(
        managed.mark(&album),
        None,
        "nothing is managed by itself here"
    );

    managed.apply(SelectionKind::Albums, &[&album], "own").await;
    let chosen: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    // The album's two editions differ only in their second id group.
    let edition = if chosen.contains("-0000-") {
        chosen.replacen("-0000-", "-0001-", 1)
    } else {
        chosen.replacen("-0001-", "-0000-", 1)
    };
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, &edition, Some("admin"))
        .await
        .expect("edition chosen");
    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album, &other], "after")
        .await;
    for item in items.iter().filter(|item| item.album_id == album) {
        assert!(item.eligibility.writes(), "{:?}", item.facts);
        assert!(item.diff.tags_changed);
    }
}

/// Undoing a person's run of the whole profile puts the hand edit it
/// wrote over back, and with it the app's record from before the run: in
/// a folder managed by hand, a wide preview after a change of edition
/// still leaves the album alone.
#[tokio::test]
async fn an_undone_profile_run_gives_the_hand_edit_back() {
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};
    use droppedneedle_library::tags::{TagField, read_fields};

    let managed = ManagedLibrary::start("lib-takeover-undo-hand-edit", |_| {}).await;
    let (other, _) = managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 1).await;
    managed.worker_looks().await;
    managed
        .apply(SelectionKind::Albums, &[&album], "album")
        .await;
    managed
        .apply(SelectionKind::Albums, &[&other], "other")
        .await;
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;
    let run = managed.apply(SelectionKind::Albums, &[&album], "own").await;

    let management = managed.management();
    let detail = management.detail(&run).expect("detail");
    let undo = management
        .undo_preview(&run, detail.job.row_revision, "undo", "admin")
        .expect("undo preview");
    assert!(
        management_step(&managed.library).await,
        "undo planning runs"
    );
    let undone = management_apply(&managed.library, &undo).await;
    assert_eq!(undone.job.state.as_str(), "succeeded");
    let file = managed.path(&tracks[0]);
    assert_eq!(
        read_fields(&file).expect("tags read")[&TagField::Title],
        vec!["By Hand".to_owned()],
        "the hand edit is back"
    );

    let chosen: String = managed
        .db()
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("edition");
    // The album's two editions differ only in their second id group.
    let edition = if chosen.contains("-0000-") {
        chosen.replacen("-0000-", "-0001-", 1)
    } else {
        chosen.replacen("-0001-", "-0000-", 1)
    };
    crate::common::library::operations(&managed.library)
        .choose_edition(&album, &edition, Some("admin"))
        .await
        .expect("edition chosen");
    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album, &other], "after")
        .await;
    for item in items.iter().filter(|item| item.album_id == album) {
        assert_eq!(item.eligibility, Eligibility::Blocked);
        assert_eq!(item.reason().expect("a reason").code, "CHANGED_OUTSIDE");
    }
}

/// A restore of the originals over more than one album leaves out a file
/// changed outside since the app wrote it and, in a folder managed
/// automatically, leaves the album unmanaged; the album restored by
/// itself goes ahead.
#[tokio::test]
async fn a_wide_restore_never_writes_over_an_unnoticed_hand_edit() {
    use droppedneedle_library::management::models::Eligibility;

    let managed = ManagedLibrary::start("lib-takeover-wide-restore", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 1).await;
    let (other, _) = managed.add_album("second", 1).await;
    managed.activate().await;
    managed.look(&album).await;
    managed.look(&other).await;
    managed
        .edit_outside(&tracks[0], title_edit("By Hand"))
        .await;

    let items = managed.restore_preview(&[&album, &other], "both").await;
    assert_eq!(items.len(), 2);
    for item in &items {
        if item.album_id == album {
            assert_eq!(item.eligibility, Eligibility::Blocked);
            assert_eq!(item.reason().expect("a reason").code, "CHANGED_OUTSIDE");
        } else {
            assert!(item.eligibility.writes(), "{:?}", item.facts);
        }
    }
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    let items = managed.restore_preview(&[&album], "own").await;
    assert!(items[0].eligibility.writes(), "{:?}", items[0].facts);
}

/// An automatic run the worker started for an album, its queue row gone,
/// is still a write to come until it has run.
#[tokio::test]
async fn an_open_automatic_run_is_a_write_to_come() {
    let managed = ManagedLibrary::start("lib-automatic-writing", |_| {}).await;
    let (album, _) = managed.add_album("first", 1).await;
    managed.activate().await;
    let management = managed.management();
    let writes = || management.writes_by_itself(&album).expect("writes read");
    managed
        .library
        .identification
        .queue_automatic_management(&album);
    assert!(writes().by_itself);
    assert!(writes().pending, "queued");
    assert!(management.automatic_next().expect("automatic step"));
    let queued: bool = managed
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM library_management_automatic_queue \
             WHERE local_album_id = ?1)",
            [&album],
            |row| row.get(0),
        )
        .expect("queue read");
    assert!(!queued, "the worker took the row");
    assert!(writes().pending, "the run is open");
    while management_step(&managed.library).await {}
    assert!(!writes().pending, "the run is done");
    assert!(writes().by_itself);
}

/// An automatic run that was waiting to be planned when a scan followed
/// its album across a hand retag and stamped it writes nothing: its plan
/// treats the stamp as the mark it is about to become.
#[tokio::test]
async fn a_run_planned_after_the_stamp_writes_nothing() {
    let managed = ManagedLibrary::start("lib-takeover-stamp-plan", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 2).await;
    managed.activate().await;
    managed
        .library
        .identification
        .queue_automatic_management(&album);
    assert!(
        managed
            .management()
            .automatic_next()
            .expect("automatic step")
    );
    managed.rename_outside(&tracks, "Renamed").await;
    assert!(managed.stamped(&album));
    let bytes: Vec<String> = tracks
        .iter()
        .map(|track| file_sha256(&managed.path(track)))
        .collect();

    while management_step(&managed.library).await {}
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    assert!(!managed.stamped(&album));
    assert_eq!(
        managed.automatic_runs().last(),
        Some(&("succeeded".to_owned(), Some("TAKEN_OVER".to_owned())))
    );
    for (track, before) in tracks.iter().zip(&bytes) {
        assert_eq!(
            &file_sha256(&managed.path(track)),
            before,
            "nothing written"
        );
        assert_eq!(album_tag(&managed.path(track)), vec!["Renamed"]);
    }
}

/// A folder that manages only acquisitions by itself can still write a
/// hand-renamed album later (a download into it), so the album is marked
/// rather than dropped with its stamp.
#[tokio::test]
async fn a_stamp_is_kept_where_the_folder_writes_other_albums() {
    let managed = ManagedLibrary::start("lib-takeover-renamed-acquisitions", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 2).await;
    managed.activate().await;
    managed.set_automatic(false, true);
    managed.rename_outside(&tracks, "Renamed").await;
    assert!(managed.stamped(&album));
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    assert_eq!(album_tag(&managed.path(&tracks[0])), vec!["Renamed"]);
    let skipped = managed.management().automatic_skipped(10).expect("skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reason.code, "TAKEN_OVER");
}

/// Every file below `dir`.
fn walk_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// The review list settles a review every way a person can: list and read
/// it, take the album's files out and put them back, and a settled review
/// refuses another decision.
#[tokio::test]
async fn library_journey_review_exclude_restore_dismiss() {
    let rig = Rig::start("lib-reviews").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-v/01.flac", "flac_full_01.flac");
    plant(&music, "album-v/02.flac", "flac_full_02.flac");
    // Nothing is scripted at MusicBrainz, so identification stays paused:
    // the journey is about what a person does with the album.
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album-v/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let conn =
        droppedneedle_kernel::db::open_connection(&app.config.library_db_path).expect("db opens");
    conn.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         VALUES ('rev-1', ?1, 'AUTOMATIC_EDITION_CLEARED_TO_REVIEW', '[]', 'pending', 1, 1)",
        [&album],
    )
    .expect("review filed");
    let availability = |conn: &rusqlite::Connection| -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT availability FROM local_tracks WHERE local_album_id = ?1")
            .expect("prepare");
        statement
            .query_map([&album], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    };

    // The list needs no album id and explains the review in plain words.
    let (status, body) = app
        .call("GET", "/api/v3/library/reviews", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["reviews"][0]["id"], json!("rev-1"));
    assert!(body["reviews"][0]["reason"]["action"].as_str().is_some());
    let (status, body) = app
        .call("GET", "/api/v3/library/reviews/rev-1", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], json!(album));

    // Exclude takes the files out; nothing is deleted.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/reviews/rev-1/exclude",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("excluded"));
    assert!(availability(&conn).iter().all(|value| value == "excluded"));
    assert!(music.join("album-v/01.flac").exists());

    // A settled review takes no other decision.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/reviews/rev-1/dismiss",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("REVIEW_SETTLED"));

    // Restore puts the files back and closes the review.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/reviews/rev-1/restore",
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("dismissed"));
    assert!(availability(&conn).iter().all(|value| value == "indexed"));
    let (_, body) = app
        .call("GET", "/api/v3/library/reviews", &headers, None)
        .await;
    assert_eq!(body["total"], json!(0), "nothing pending");
    let (_, body) = app
        .call("GET", "/api/v3/library/reviews?state=all", &headers, None)
        .await;
    assert_eq!(body["total"], json!(1));

    // A bulk keep-as-tagged previews first, and applies only that preview.
    conn.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         VALUES ('rev-2', ?1, 'EDITION_CLEARED_CONFLICT', '[]', 'pending', 2, 2)",
        [&album],
    )
    .expect("second review filed");
    let selection = json!({"action": "keep_tagged", "filter": {"root_id": "music"}});
    let (status, preview) = app
        .call(
            "POST",
            "/api/v3/library/reviews/bulk-preview",
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["eligible"], json!(1));
    let mut stale = selection.clone();
    stale["preview_token"] = json!("not-the-preview");
    let (status, _) = app
        .call(
            "POST",
            "/api/v3/library/reviews/bulk-apply",
            &headers,
            Some(stale),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/reviews/bulk-apply",
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], json!(1));
    let (_, body) = app
        .call("GET", "/api/v3/library/reviews/rev-2", &headers, None)
        .await;
    assert_eq!(body["state"], json!("rejected"));

    // A bulk accept never replaces an edition someone picked by hand.
    conn.execute(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) \
         VALUES (?1, 'musicbrainz', 'rg-pinned', 'manual', 1.0)",
        [&album],
    )
    .expect("pinned");
    let candidates = json!([{
        "candidate_key": "rg-other:", "release_group_mbid": "rg-other", "release_mbid": null,
        "album_title": "Other", "album_artist_name": "Someone", "track_evidence": [],
        "score": 0.9, "reason_code": "WEAK_MATCH"
    }]);
    conn.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_ms, updated_ms) \
         VALUES ('rev-3', ?1, 'WEAK_MATCH', ?2, 'pending', 3, 3)",
        rusqlite::params![album, candidates.to_string()],
    )
    .expect("third review filed");
    let (status, preview) = app
        .call(
            "POST",
            "/api/v3/library/reviews/bulk-preview",
            &headers,
            Some(json!({"action": "accept", "filter": {}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["eligible"], json!(0));
    assert_eq!(
        preview["skipped"][0]["reason"]["code"],
        json!("REVIEW_ALBUM_PINNED")
    );
    rig.stop().await;
}

/// Approving an edition that names only a release queues the placing of
/// the album's files. Until that is done a manage preview of the album is
/// refused with a message that says so; once the queue is clear it goes
/// through.
#[tokio::test]
async fn manage_preview_waits_while_the_chosen_edition_is_placed() {
    let rig = Rig::start("lib-remap-preview").await;
    rig.identify.recall_from(ambiguous_from);
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let music = app.config.root_app_dir.join("music");
    plant(&music, "album-w/01.flac", "management_full.flac");
    app.add_library_folder("music", &music).await;
    let track_id = rig
        .scan_store
        .track_at("music", "album-w/01.flac")
        .unwrap()
        .expect("track indexed");
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{track_id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let album = body["album_id"].as_str().expect("album id").to_owned();
    let album_key = album.as_str();
    rig.identified(album_key, 1).await;
    file_edition_review(&rig.identify_store, album_key);
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/reviews?album_id={album_key}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let review_id = body["reviews"][0]["id"]
        .as_str()
        .expect("review")
        .to_owned();
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/reviews/{review_id}/approve"),
            &headers,
            Some(json!({"candidate_key": format!("rg-1:{REL_1}")})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.edition_settled(album_key).await;

    // The queue row an approval leaves while the files are placed. It is
    // seeded by hand with a far-off `not_before`, so the loop never takes it.
    let db = app.db();
    db.execute(
        "INSERT INTO library_edition_remap_queue \
         (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
         VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
        rusqlite::params![album_key, REL_1],
    )
    .expect("remap row queued");
    let preview = json!({
        "kind": "retag",
        "album_id": album_key,
        "items": [{
            "root_id": "music",
            "rel_path": "album-w/01.flac",
            "managed_updates": {"title": ["Waiting Title"]},
        }],
    });
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(preview.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        json!("Still matching this album to the edition you chose. Try again in a moment.")
    );

    db.execute(
        "UPDATE library_edition_remap_queue SET attempts = 1 WHERE local_album_id = ?1",
        [album_key],
    )
    .expect("row updated");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(preview.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        json!(
            "Still matching this album to the edition you chose. \
             MusicBrainz is not answering, so this waits until it does."
        )
    );

    db.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        [album_key],
    )
    .expect("row removed");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(preview),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.stop().await;
}

/// A Library Management preview a person starts does not plan an album
/// whose chosen edition is still being placed: each of its files is blocked
/// with EDITION_REMAP_PENDING, and the other album of the preview plans.
#[tokio::test]
async fn management_preview_blocks_an_album_whose_edition_is_being_placed() {
    use droppedneedle_library::management::models::{ItemFilter, Selection, SelectionKind};
    use droppedneedle_library::management::service::PreviewRequest;
    use droppedneedle_library::management::settings as lm;

    let managed = ManagedLibrary::start("lib-remap-management", |_| {}).await;
    let (waiting, _) = managed.add_album("waiting", 2).await;
    let (other, _) = managed.add_album("other", 2).await;
    managed
        .db()
        .execute(
            "INSERT INTO library_edition_remap_queue \
             (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
             VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
            rusqlite::params![waiting, REL_1],
        )
        .expect("remap row queued");
    let management = managed.management();
    let handle = management
        .create_preview(
            PreviewRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![waiting.clone(), other.clone()],
                    catalog_filter: None,
                },
                profile_id: lm::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                expected_settings_revision: managed.settings_revision(),
                expected_policy_revision: managed
                    .library
                    .core
                    .policy_revision()
                    .expect("policy revision"),
                idempotency_key: Some("preview-remap".to_owned()),
                target_root_id: None,
                overrides: None,
            },
            "admin",
        )
        .expect("preview queued");
    assert!(management_step(&managed.library).await, "planning runs");
    let page = management
        .items(&handle.job_id, -1, 100, &ItemFilter::default())
        .expect("items page");
    assert_eq!(page.items.len(), 4, "two files of each album");
    for item in &page.items {
        let reason = item.facts.reason.as_ref();
        if item.album_id == waiting {
            let reason = reason.expect("the waiting album has a reason");
            assert_eq!(reason.code, "EDITION_REMAP_PENDING");
            assert_eq!(
                reason.message,
                "Still matching this album to the edition you chose."
            );
            assert_eq!(reason.action, "Try again in a moment.");
        } else {
            assert_eq!(reason, None, "the other album plans as before");
        }
    }
}

/// Automatic management is not held back by a queued edition: the run
/// goes ahead and no file carries EDITION_REMAP_PENDING.
#[tokio::test]
async fn automatic_run_is_not_held_by_an_edition_being_placed() {
    use droppedneedle_library::management::models::ItemFilter;

    let managed = ManagedLibrary::start("lib-remap-automatic", |_| {}).await;
    let (album, _) = managed.add_album("automatic", 2).await;
    managed
        .db()
        .execute(
            "INSERT INTO library_edition_remap_queue \
             (local_album_id, release_mbid, queued_at, attempts, not_before, origin) \
             VALUES (?1, ?2, 0, 0, 1e12, 'album_pin')",
            rusqlite::params![album, REL_1],
        )
        .expect("remap row queued");
    managed.activate().await;
    managed.look(&album).await;
    let last = managed.automatic_runs().pop().expect("a run");
    assert_eq!(last.0, "succeeded");
    let management = managed.management();
    let (history, _) = management.history(Default::default()).expect("history");
    let run = history
        .iter()
        .find(|item| item.origin == "scan_discovered")
        .expect("an automatic run");
    let page = management
        .items(&run.job.id, -1, 100, &ItemFilter::default())
        .expect("items page");
    assert!(!page.items.is_empty(), "the run planned the album");
    assert!(page.items.iter().all(|item| {
        item.facts
            .reason
            .as_ref()
            .is_none_or(|reason| reason.code != "EDITION_REMAP_PENDING")
    }));
}

/// One release, `REL_1`, with a track for each title among the files (a
/// second copy of a file adds none), numbered as the files are. Its
/// release tracks are `rt-<track number>`.
fn one_track_per_title(facts: &LocalAlbumFacts) -> RecallResult {
    let mut seen = std::collections::BTreeSet::new();
    let tracks = facts
        .tracks
        .iter()
        .filter(|track| seen.insert(track.title.clone()))
        .enumerate()
        .map(|(index, track)| ReleaseTrack {
            isrcs: Vec::new(),
            genres: Vec::new(),
            id: format!("rt-{}", track.track_number),
            recording_id: track
                .recording_mbid
                .clone()
                .unwrap_or_else(|| format!("rec-{}", track.track_number)),
            title: track.title.clone(),
            artists: Vec::new(),
            disc: track.disc_number.max(1),
            position: track.track_number,
            absolute_position: index as u32 + 1,
            length_ms: track.duration_secs.map(|seconds| seconds * 1000),
        })
        .collect();
    RecallResult {
        releases: vec![Release {
            id: REL_1.to_owned(),
            release_group_id: "rg-1".to_owned(),
            title: facts.title.clone(),
            artists: vec![CreditedArtist {
                id: "artist-1".to_owned(),
                name: facts.album_artist_name.clone(),
                sort_name: None,
                join: String::new(),
            }],
            tracks,
            ..Release::default()
        }],
        ..RecallResult::default()
    }
}

/// An album's identity row as a person's choice reads it: source,
/// release, chooser, when it was chosen and its revision.
type ChoiceRow = (String, Option<String>, Option<String>, f64, i64);

fn choice_row(db: &rusqlite::Connection, album: &str) -> ChoiceRow {
    db.query_row(
        "SELECT decision_source, release_mbid, selected_by_user_id, selected_at, row_revision \
         FROM local_album_external_identities WHERE local_album_id = ?1",
        [album],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .expect("identity row")
}

/// A track's placement: release, release track and revision.
fn placement(db: &rusqlite::Connection, track: &str) -> (Option<String>, Option<String>, i64) {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT release_mbid, release_track_mbid, row_revision \
         FROM local_track_external_identities WHERE local_track_id = ?1",
        [track],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .expect("placement reads")
    .unwrap_or((None, None, 0))
}

/// A merge into an album whose edition someone chose (by hand, or carried
/// from v2) puts the merged files on that edition in the background and
/// changes nothing else: who chose it and when, the identity's revision,
/// "leave unmanaged", the taken-over mark and the scan's stamp stay. A file
/// already on the edition keeps its row; a merged-in second copy of it
/// stays unplaced rather than take its release track.
#[tokio::test]
async fn a_merge_places_files_on_the_edition_the_album_has() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    for source in ["manual", "legacy_import"] {
        let rig = Rig::start(&format!("lib-merge-place-{source}")).await;
        let app = &rig.app;
        let auth = app.login_admin().await;
        let headers = [("authorization", auth.as_str())];
        let music = app.config.root_app_dir.join("music");
        plant(&music, "a/01.flac", "flac_full_01.flac");
        plant(&music, "a/02.flac", "flac_full_02.flac");
        plant(&music, "b/01.flac", "flac_full_01.flac");
        let third = plant(&music, "b/03.flac", "flac_full_02.flac");
        save_tags(
            &third,
            &[
                TagEdit::verbatim(TagField::Title, vec!["Three".to_owned()]),
                TagEdit::verbatim(TagField::TrackNumber, vec!["3".to_owned()]),
                TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
                TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
            ],
        )
        .expect("third track tagged");
        rig.identify.recall_from(one_track_per_title);
        rig.pause_identification().await;
        app.add_library_folder("music", &music).await;
        app.scan_library().await;
        let store = &rig.scan_store;
        let track = |rel: &str| {
            store
                .track_at("music", rel)
                .unwrap()
                .expect("track indexed")
        };
        let (a1, a2, b1, b3) = (
            track("a/01.flac"),
            track("a/02.flac"),
            track("b/01.flac"),
            track("b/03.flac"),
        );
        let album_a = store.album_for_track(&a1).unwrap().expect("album a");
        let album_b = store.album_for_track(&b1).unwrap().expect("album b");
        assert_ne!(album_a, album_b, "{source}");
        assert_eq!(
            store.album_for_track(&b3).unwrap().as_deref(),
            Some(album_b.as_str())
        );

        let db = app.db();
        db.execute_batch(&format!(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision) VALUES ('{album_a}', 'musicbrainz', 'rg-1', '{REL_1}', \
             '{source}', 'curator', 1234.5, 7); \
             INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source, selected_at) VALUES \
             ('{a1}', 'musicbrainz', 'rec-a1', '{REL_1}', 'rt-1', 1, 1, 'manual', 1), \
             ('{a2}', 'musicbrainz', 'rec-a2', '{REL_1}', 'rt-2', 1, 2, 'manual', 1), \
             ('{b1}', 'musicbrainz', 'rec-b1', '{REL_2}', 'other-1', 1, 1, 'automatic', 1), \
             ('{b3}', 'musicbrainz', 'rec-b3', '{REL_2}', 'other-3', 1, 3, 'automatic', 1); \
             INSERT INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) VALUES ('{album_a}', 'leave_unmanaged', \
             'curator', 1); \
             INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
             VALUES ('{album_a}', 'files_changed', 1234); \
             INSERT INTO library_management_automatic_queue (local_album_id, queued_at, \
             not_before, retag_followed_at) VALUES ('{album_a}', 1, 1, 5678);"
        ))
        .expect("album a chosen and left unmanaged");
        let chosen = choice_row(&db, &album_a);
        let held = [placement(&db, &a1), placement(&db, &a2)];

        let selection = json!({"track_ids": [b1, b3], "target_album_id": album_a});
        let (status, preview) = app
            .call(
                "POST",
                "/api/v3/library/albums/merge-preview",
                &headers,
                Some(selection.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert!(
            preview["edition_changes"]
                .as_array()
                .expect("edition changes")
                .iter()
                .any(|change| change["change"] == json!("remap_queued")),
            "{source}: the preview says the files will be placed: {preview}"
        );
        let mut apply = selection;
        apply["preview_token"] = preview["preview_token"].clone();
        let (status, body) = app
            .call(
                "POST",
                "/api/v3/library/albums/merge",
                &headers,
                Some(apply),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        rig.edition_settled(&album_a).await;

        let (release, release_track, _) = placement(&db, &b3);
        assert_eq!(
            (release.as_deref(), release_track.as_deref()),
            (Some(REL_1), Some("rt-3")),
            "{source}: the merged file is on the edition"
        );
        assert_eq!(
            [placement(&db, &a1), placement(&db, &a2)],
            held,
            "{source}: the files already on it keep their rows"
        );
        assert_eq!(
            placement(&db, &b1).0,
            None,
            "{source}: a second copy stays unplaced"
        );
        assert_eq!(choice_row(&db, &album_a), chosen, "{source}");
        let kept: (i64, i64, Option<i64>) = db
            .query_row(
                "SELECT (SELECT COUNT(*) FROM library_management_exclusions \
                 WHERE local_album_id = ?1), \
                 (SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = ?1), \
                 (SELECT retag_followed_at FROM library_management_automatic_queue \
                 WHERE local_album_id = ?1)",
                [&album_a],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("album a reads");
        assert_eq!(
            kept,
            (1, 1, Some(5678)),
            "{source}: leave unmanaged, the mark and the stamp stay"
        );
        rig.stop().await;
    }
}

/// A file that joins the album while MusicBrainz is being asked was not
/// weighed by the answer: it keeps its row and is placed on the next try,
/// never cleared as a file the edition has no track for.
#[tokio::test]
async fn a_file_that_joins_while_musicbrainz_is_asked_is_placed_next() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let rig = Rig::start("lib-place-late-file").await;
    let app = &rig.app;
    let music = app.config.root_app_dir.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "a/02.flac", "flac_full_02.flac");
    let third = plant(&music, "b/03.flac", "flac_full_02.flac");
    save_tags(
        &third,
        &[
            TagEdit::verbatim(TagField::Title, vec!["Three".to_owned()]),
            TagEdit::verbatim(TagField::TrackNumber, vec!["3".to_owned()]),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
        ],
    )
    .expect("third track tagged");
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    app.scan_library().await;
    let store = &rig.scan_store;
    let track = |rel: &str| {
        store
            .track_at("music", rel)
            .unwrap()
            .expect("track indexed")
    };
    let (a1, a2, b3) = (track("a/01.flac"), track("a/02.flac"), track("b/03.flac"));
    let album = store.album_for_track(&a1).unwrap().expect("album");
    let db = app.db();
    db.execute_batch(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at, row_revision) VALUES ('{album}', 'musicbrainz', 'rg-1', '{REL_1}', \
         'manual', 'curator', 1234.5, 7); \
         INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source, selected_at) VALUES \
         ('{a1}', 'musicbrainz', 'rec-a1', '{REL_1}', 'rt-1', 1, 1, 'manual', 1), \
         ('{a2}', 'musicbrainz', 'rec-a2', '{REL_1}', 'rt-2', 1, 2, 'manual', 1), \
         ('{b3}', 'musicbrainz', 'rec-b3', '{REL_2}', 'other-3', 1, 3, 'automatic', 1);"
    ))
    .expect("edition chosen");
    let held = [placement(&db, &a1), placement(&db, &a2)];
    // The first answer is made from the album as it was asked about; the
    // third file joins the album while that answer is on its way.
    let joined = Arc::new(AtomicBool::new(false));
    let db_path = app.config.library_db_path.clone();
    let (late, target) = (b3.clone(), album.clone());
    rig.identify.recall_from(move |facts| {
        if !joined.swap(true, Ordering::SeqCst) {
            droppedneedle_kernel::db::open_connection(&db_path)
                .expect("db opens")
                .execute(
                    "UPDATE local_tracks SET local_album_id = ?1 WHERE id = ?2",
                    [&target, &late],
                )
                .expect("the file joins the album");
        }
        one_track_per_title(facts)
    });
    db.execute(
        "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, queued_at, \
         origin) VALUES (?1, ?2, 1, 'place_only')",
        rusqlite::params![album, REL_1],
    )
    .expect("files queued");
    rig.edition_settled(&album).await;

    let (release, release_track, _) = placement(&db, &b3);
    assert_eq!(
        (release.as_deref(), release_track.as_deref()),
        (Some(REL_1), Some("rt-3")),
        "the file that joined late is placed"
    );
    assert_eq!([placement(&db, &a1), placement(&db, &a2)], held);
    rig.stop().await;
}

/// A two-track album scanned into a library with no app around it, and a
/// `curator` who chose its edition. Returns the scratch folder, the
/// library, the album and its tracks.
async fn chosen_album_at_rest(tag: &str) -> (Scratch, LibrarySetup, String, Vec<String>) {
    let (scratch, library, music) = bare_library(tag).await;
    plant(&music, "album/01.flac", "flac_full_01.flac");
    plant(&music, "album/02.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let tracks: Vec<String> = ["album/01.flac", "album/02.flac"]
        .iter()
        .map(|rel| {
            library
                .scan_store
                .track_at("music", rel)
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album = library
        .scan_store
        .album_for_track(&tracks[0])
        .unwrap()
        .expect("album");
    library
        .scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision) VALUES ('{album}', 'musicbrainz', 'rg-1', '{REL_1}', \
             'manual', 'curator', 1234.5, 7);"
        ))
        .expect("edition chosen");
    (scratch, library, album, tracks)
}

fn scratch_db(scratch: &Scratch) -> rusqlite::Connection {
    droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens")
}

/// Queue the album's files for `REL_1` with `origin`.
fn queue_remap_row(library: &LibrarySetup, album: &str, origin: &str) {
    library
        .scan_store
        .db()
        .run_sql(&format!(
            "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
             queued_at, origin) VALUES ('{album}', '{REL_1}', 1, '{origin}');"
        ))
        .expect("remap queued");
}

/// Answer MusicBrainz with `recall` and run the remap worker once.
async fn remap_once(library: &LibrarySetup, identify: &FakeProviders, recall: RecallResult) {
    identify.set_recall(recall);
    assert!(
        crate::common::library::operations(library)
            .remap_next()
            .await
            .expect("remap step"),
        "a queued row was handled"
    );
}

/// The album's remap row, as (origin, attempts), if any.
fn remap_row(db: &rusqlite::Connection, album: &str) -> Option<(String, i64)> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT origin, attempts FROM library_edition_remap_queue WHERE local_album_id = ?1",
        [album],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .expect("queue reads")
}

/// The album's match state reason, if any.
fn match_state(db: &rusqlite::Connection, album: &str) -> Option<String> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT reason_code FROM library_album_match_state WHERE local_album_id = ?1",
        [album],
        |row| row.get(0),
    )
    .optional()
    .expect("state reads")
}

/// The album's files as MusicBrainz would offer them for `REL_1`.
fn fitting_release(library: &LibrarySetup, album: &str) -> RecallResult {
    one_track_per_title(
        &library
            .identify_store
            .album_facts(album)
            .unwrap()
            .expect("facts"),
    )
}

/// A release MusicBrainz lists under `REL_1` that none of the files fit.
fn unrelated_release(library: &LibrarySetup, album: &str) -> RecallResult {
    let mut recall = fitting_release(library, album);
    for (index, track) in recall.releases[0].tracks.iter_mut().enumerate() {
        track.title = format!("Nothing alike {index}");
        track.recording_id = format!("rec-elsewhere-{index}");
        track.position += 20;
        track.length_ms = Some(3_600_000);
    }
    recall
}

/// Placing the files on an edition an album already has never gives the
/// edition up. MusicBrainz not listing it any more, or listing it with
/// tracks no file fits, leaves the person's match as it was and says why
/// on the album; MusicBrainz not answering waits for another try.
#[tokio::test]
async fn placing_files_never_gives_the_edition_up() {
    let (scratch, library, album, _) = chosen_album_at_rest("lib-place-not-listed").await;
    let mut db = scratch_db(&scratch);
    let chosen = choice_row(&db, &album);
    for _ in 0..2 {
        let tx = db.transaction().expect("transaction opens");
        assert!(
            droppedneedle_library::operations::choice::queue_placement(&tx, &album, 1.0)
                .expect("files queued")
        );
        tx.commit().expect("queued for good");
    }
    assert_eq!(
        remap_row(&db, &album),
        Some(("place_only".to_owned(), 0)),
        "queued twice is one row, still place_only"
    );
    remap_once(&library, &scratch.identify, RecallResult::default()).await;
    assert_eq!(remap_row(&db, &album), None);
    assert_eq!(choice_row(&db, &album), chosen, "the person's match stays");
    assert_eq!(
        match_state(&db, &album).as_deref(),
        Some("CHOSEN_EDITION_NOT_LISTED")
    );

    let (scratch, library, album, _) = chosen_album_at_rest("lib-place-fits-nothing").await;
    let db = scratch_db(&scratch);
    let chosen = choice_row(&db, &album);
    queue_remap_row(&library, &album, "place_only");
    let recall = unrelated_release(&library, &album);
    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    assert_eq!(choice_row(&db, &album), chosen, "the person's match stays");
    assert_eq!(
        match_state(&db, &album).as_deref(),
        Some("CHOSEN_EDITION_FITS_NO_FILE")
    );

    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-unavailable").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "place_only");
    remap_once(
        &library,
        &scratch.identify,
        RecallResult {
            provider_deferred: true,
            ..RecallResult::default()
        },
    )
    .await;
    assert_eq!(remap_row(&db, &album), Some(("place_only".to_owned(), 1)));
    assert_eq!(match_state(&db, &album), None);
    assert!(
        tracks.iter().all(|track| placement(&db, track).0.is_none()),
        "nothing placed while MusicBrainz is away"
    );
}

/// A person's edition whose MusicBrainz answer does not read is kept: the
/// remap waits for another try instead of handing the album back to
/// automatic best fit.
#[tokio::test]
async fn a_pinned_edition_that_does_not_read_keeps_the_choice() {
    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-pin-unreadable").await;
    let db = scratch_db(&scratch);
    let chosen = choice_row(&db, &album);
    queue_remap_row(&library, &album, "album_pin");
    remap_once(
        &library,
        &scratch.identify,
        RecallResult {
            failure_code: Some(
                droppedneedle_library::identify::providers::RELEASE_UNREADABLE.to_owned(),
            ),
            ..RecallResult::default()
        },
    )
    .await;
    assert_eq!(remap_row(&db, &album), Some(("album_pin".to_owned(), 1)));
    assert_eq!(choice_row(&db, &album), chosen, "the person's match stays");
    assert_eq!(match_state(&db, &album), None);
    assert!(
        tracks.iter().all(|track| placement(&db, track).0.is_none()),
        "nothing placed"
    );
}

/// Files queued to be placed on a person's edition are not placed once the
/// album's match has been made automatic since: the row goes unrun.
#[tokio::test]
async fn a_match_made_automatic_since_drops_its_queued_placement() {
    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-made-automatic").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "place_only");
    db.execute(
        "UPDATE local_album_external_identities SET decision_source = 'automatic', \
         selected_by_user_id = NULL WHERE local_album_id = ?1",
        [&album],
    )
    .expect("made automatic");
    let recall = fitting_release(&library, &album);
    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    assert!(tracks.iter().all(|track| placement(&db, track).0.is_none()));
}

/// A person's choice queued before their later "leave unmanaged" (a match
/// naming only the release group, and an exclusion) is dropped unrun: the
/// old release is not sealed back, the exclusion stays and no file is
/// placed.
#[tokio::test]
async fn leave_unmanaged_since_drops_a_queued_choice() {
    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-left-unmanaged").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "album_pin");
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET release_mbid = NULL, \
         row_revision = row_revision + 1 WHERE local_album_id = '{album}'; \
         INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES ('{album}', 'leave_unmanaged', \
         'curator', 1);"
    ))
    .expect("left unmanaged");
    let left = choice_row(&db, &album);
    let recall = fitting_release(&library, &album);
    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    assert_eq!(
        choice_row(&db, &album),
        left,
        "the old release is not sealed back"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = ?1",
            [&album],
            |row| row.get::<_, i64>(0),
        )
        .expect("exclusion reads"),
        1
    );
    assert!(tracks.iter().all(|track| placement(&db, track).0.is_none()));
}

/// A person's queued choice that runs after the album gained a "leave
/// unmanaged" (carried by a merge while MusicBrainz was away) places the
/// files and keeps the album unmanaged.
#[tokio::test]
async fn a_queued_choice_keeps_a_carried_leave_unmanaged() {
    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-keeps-hold").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "album_pin");
    db.execute(
        "INSERT INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) VALUES (?1, 'leave_unmanaged', 'curator', 1)",
        [&album],
    )
    .expect("left unmanaged");
    let recall = fitting_release(&library, &album);
    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    assert!(
        tracks
            .iter()
            .all(|track| placement(&db, track).0.as_deref() == Some(REL_1))
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = ?1",
            [&album],
            |row| row.get::<_, i64>(0),
        )
        .expect("exclusion reads"),
        1
    );
}

/// A curator approving a release, with the evidence for each file, makes
/// it their choice and queues the files to be placed on it, replacing what
/// was queued (here a release-group pin's row). The approval itself leaves
/// the files' rows alone. Placing the files then puts each on its release
/// track with its position and changes nothing of the album's match, so
/// the approval's undo still puts the album back as it was.
#[tokio::test]
async fn an_approved_release_is_placed_and_its_undo_survives() {
    use droppedneedle_library::identify::models::{
        CandidateEvidence, EvidenceClass, TrackEvidence,
    };

    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-approved").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "group_pin");
    let chosen = choice_row(&db, &album);
    let files: Vec<_> = tracks.iter().map(|track| placement(&db, track)).collect();
    let mut recall = fitting_release(&library, &album);
    recall.releases[0].id = REL_2.to_owned();
    recall.releases[0].release_group_id = "rg-2".to_owned();
    let release = recall.releases[0].clone();
    droppedneedle_library::identify::review::file_review(
        &library.identify_store,
        "rev-approved",
        &album,
        "EDITION_UNCERTAIN",
        vec![CandidateEvidence {
            candidate_key: format!("rg-2:{REL_2}"),
            release_group_mbid: "rg-2".to_owned(),
            release_mbid: Some(REL_2.to_owned()),
            album_title: "Album".to_owned(),
            album_artist_name: String::new(),
            track_evidence: tracks
                .iter()
                .zip(&release.tracks)
                .map(|(track, on_release)| TrackEvidence {
                    local_track_id: track.clone(),
                    classification: EvidenceClass::Supported,
                    evidence_kinds: Vec::new(),
                    recording_mbid: Some(on_release.recording_id.clone()),
                    release_track_mbid: Some(on_release.id.clone()),
                })
                .collect(),
            score: 1.0,
            reason_code: "EDITION_UNCERTAIN".to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        }],
    )
    .unwrap();

    library
        .identification
        .approve_review("rev-approved", "curator", &format!("rg-2:{REL_2}"))
        .expect("approved");
    assert_eq!(remap_row(&db, &album), Some(("album_pin".to_owned(), 0)));
    let after: Vec<_> = tracks.iter().map(|track| placement(&db, track)).collect();
    assert_eq!(after, files, "the approval leaves the files");

    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    for (track, on_release) in tracks.iter().zip(&release.tracks) {
        let placed: (
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            String,
        ) = db
            .query_row(
                "SELECT release_mbid, release_track_mbid, medium_position, \
                 release_track_position, decision_source \
                 FROM local_track_external_identities WHERE local_track_id = ?1",
                [track],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("placed");
        assert_eq!(
            placed,
            (
                Some(REL_2.to_owned()),
                Some(on_release.id.clone()),
                Some(i64::from(on_release.disc)),
                Some(i64::from(on_release.position)),
                "manual".to_owned()
            )
        );
    }

    crate::common::library::operations(&library)
        .undo_edition_choice(&album, "curator")
        .expect("the approval's undo still applies");
    let undone = choice_row(&db, &album);
    assert_eq!(
        (&undone.0, &undone.1, &undone.2, undone.3),
        (&chosen.0, &chosen.1, &chosen.2, chosen.3),
        "the album is as before the approval"
    );
    let files_now: Vec<_> = tracks.iter().map(|track| placement(&db, track)).collect();
    assert_eq!(
        files_now
            .iter()
            .map(|row| (&row.0, &row.1))
            .collect::<Vec<_>>(),
        files.iter().map(|row| (&row.0, &row.1)).collect::<Vec<_>>(),
        "the files are as before"
    );
}

/// A chosen edition MusicBrainz now lists under a newer id: placing the
/// files writes them on the release as the album's match names it, so
/// what reads the album's release finds them.
#[tokio::test]
async fn a_choice_placed_under_a_merged_id_keeps_the_albums_release() {
    let (scratch, library, album, tracks) = chosen_album_at_rest("lib-place-merged").await;
    let db = scratch_db(&scratch);
    queue_remap_row(&library, &album, "album_pin");
    let mut recall = fitting_release(&library, &album);
    recall.releases[0].id = REL_2.to_owned();
    recall.releases[0].old_ids = vec![REL_1.to_owned()];

    remap_once(&library, &scratch.identify, recall).await;
    assert_eq!(remap_row(&db, &album), None);
    for track in &tracks {
        assert_eq!(
            placement(&db, track).0.as_deref(),
            Some(REL_1),
            "on the album's release"
        );
    }
}

/// An album retagged by hand across two scans, whose edition a person
/// chose and whose files the app never wrote, is joined with that edition.
/// Placing the files on it and the automatic look that follows write no
/// file and leave the match as it was; the album is left unmanaged.
#[tokio::test]
async fn a_joined_album_gets_its_files_placed_and_stays_the_users() {
    let managed = ManagedLibrary::start("lib-place-joined", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 4).await;
    managed.activate().await;
    managed
        .db()
        .execute(
            "UPDATE local_track_external_identities SET release_mbid = 'rel-elsewhere' \
             WHERE local_track_id IN (?1, ?2)",
            [&tracks[0], &tracks[1]],
        )
        .expect("two files on another release");
    assert_eq!(
        managed.album_of(&tracks[0]),
        album,
        "one album before the retag"
    );
    let bytes = || -> Vec<String> {
        tracks
            .iter()
            .map(|track| file_sha256(&managed.path(track)))
            .collect()
    };

    managed.rename_outside(&tracks[..2], "Renamed").await;
    let new = managed.album_of(&tracks[0]);
    assert_ne!(new, album, "the first half sits on a row of its own");
    managed.rename_outside(&tracks[2..], "Renamed").await;
    assert_eq!(managed.album_of(&tracks[3]), new, "joined");
    let edited = bytes();
    let db = managed.db();
    let chosen = choice_row(&db, &new);
    assert_eq!(chosen.0, "manual");

    assert!(
        crate::common::library::operations(&managed.library)
            .remap_next()
            .await
            .expect("remap step"),
        "the files wait to be placed"
    );
    assert_eq!(remap_row(&db, &new), None);
    for track in &tracks {
        assert_eq!(
            placement(&db, track).0,
            chosen.1,
            "every file is on the edition"
        );
    }
    managed.worker_looks().await;
    assert_eq!(managed.mark(&new).as_deref(), Some("retag_followed"));
    assert_eq!(bytes(), edited, "nothing written");
    assert_eq!(choice_row(&db, &new), chosen, "the match is as it was");
}

/// A dry run set aside for another one, here made before a rule was
/// changed, can never be applied: an album retagged across two scans is
/// joined although that dry run planned the album.
#[tokio::test]
async fn a_replaced_dry_run_does_not_hold_a_join_back() {
    let managed = ManagedLibrary::start("lib-replaced-dry-run", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 4).await;
    managed
        .dry_run("set-aside", |settings| {
            settings.preview_retention_hours = 48
        })
        .await;
    managed.activate().await;
    let db = managed.db();
    db.execute(
        "UPDATE local_track_external_identities SET release_mbid = 'rel-elsewhere' \
         WHERE local_track_id IN (?1, ?2)",
        [&tracks[0], &tracks[1]],
    )
    .expect("two files on another release");

    managed.rename_outside(&tracks[..2], "Renamed").await;
    let new = managed.album_of(&tracks[0]);
    assert_ne!(new, album, "the first half sits on a row of its own");
    managed.rename_outside(&tracks[2..], "Renamed").await;
    assert_eq!(managed.album_of(&tracks[3]), new);
    assert_eq!(choice_row(&db, &new).0, "manual", "joined with its match");
}

/// A preview left ready past its expiry is nobody's work: an album
/// retagged across two scans is joined although the preview planned it.
#[tokio::test]
async fn an_expired_preview_does_not_hold_a_join_back() {
    let managed = ManagedLibrary::start("lib-expired-preview", |_| {}).await;
    managed.add_album("first", 1).await;
    let (album, tracks) = managed.add_album("second", 4).await;
    managed.activate().await;
    let db = managed.db();
    let left_ready = db
        .execute(
            "UPDATE library_operation_jobs SET state = 'ready', terminal_code = NULL, \
             terminal_at = NULL WHERE id IN (SELECT job_id FROM \
             library_management_job_snapshots WHERE proposed_settings_revision IS NOT NULL)",
            [],
        )
        .expect("preview left ready");
    assert_eq!(left_ready, 1, "one preview");
    db.execute(
        "UPDATE library_management_job_snapshots SET phase = 'ready', \
         preview_expires_at = 1, proposed_settings_revision = NULL \
         WHERE proposed_settings_revision IS NOT NULL",
        [],
    )
    .expect("an ordinary preview, expired");
    db.execute(
        "UPDATE local_track_external_identities SET release_mbid = 'rel-elsewhere' \
         WHERE local_track_id IN (?1, ?2)",
        [&tracks[0], &tracks[1]],
    )
    .expect("two files on another release");

    managed.rename_outside(&tracks[..2], "Renamed").await;
    let new = managed.album_of(&tracks[0]);
    assert_ne!(new, album, "the first half sits on a row of its own");
    managed.rename_outside(&tracks[2..], "Renamed").await;
    assert_eq!(managed.album_of(&tracks[3]), new);
    assert_eq!(choice_row(&db, &new).0, "manual", "joined with its match");
}

// ---------------------------------------------------------------------------
// A track retagged and renamed keeps what people made of it.
// ---------------------------------------------------------------------------

/// An album of `count` tracks scanned into a library with no app around
/// it, `album/01.flac` onwards, each with its own title and number.
/// Returns the scratch folder, the library, the music folder, the album
/// and its tracks.
async fn album_at_rest(
    tag: &str,
    count: usize,
) -> (Scratch, LibrarySetup, PathBuf, String, Vec<String>) {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let (scratch, library, music) = bare_library(tag).await;
    // Every file's release group id is cleared: the fixtures' would name
    // another album than the one the tests script.
    for number in 1..=count {
        let fixture = if number == 1 {
            "flac_full_01.flac"
        } else {
            "flac_full_02.flac"
        };
        let file = plant(&music, &format!("album/{number:02}.flac"), fixture);
        let mut edits = vec![TagEdit::verbatim(
            TagField::MusicBrainzReleaseGroupId,
            Vec::new(),
        )];
        if number > 2 {
            edits.extend([
                TagEdit::verbatim(TagField::Title, vec![format!("Track {number}")]),
                TagEdit::verbatim(TagField::TrackNumber, vec![number.to_string()]),
                TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
                TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
            ]);
        }
        save_tags(&file, &edits).expect("track tagged");
    }
    add_music_root(&library, &music).await;
    let tracks: Vec<String> = (1..=count)
        .map(|number| {
            library
                .scan_store
                .track_at("music", &format!("album/{number:02}.flac"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album = library
        .scan_store
        .album_for_track(&tracks[0])
        .unwrap()
        .expect("album");
    (scratch, library, music, album, tracks)
}

/// Retag `album/NN.flac` to the album `title`, with or without its
/// MusicBrainz ids, and when `rename` give it a new name in its folder.
/// Returns where it is now.
fn retag_outside(music: &Path, number: usize, title: &str, keep_ids: bool, rename: bool) -> String {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let from = format!("album/{number:02}.flac");
    let mut edits = vec![TagEdit::verbatim(TagField::Album, vec![title.to_owned()])];
    if !keep_ids {
        edits.extend(
            [
                TagField::MusicBrainzReleaseId,
                TagField::MusicBrainzReleaseGroupId,
                TagField::MusicBrainzReleaseTrackId,
                TagField::MusicBrainzRecordingId,
            ]
            .map(|field| TagEdit::verbatim(field, Vec::new())),
        );
    }
    save_tags(&music.join(&from), &edits).expect("retagged outside the app");
    if !rename {
        return from;
    }
    let to = format!("album/{number:02} renamed.flac");
    std::fs::rename(music.join(&from), music.join(&to)).expect("renamed outside the app");
    to
}

/// The album row a track is on, whether its file is there or not.
fn album_row(db: &rusqlite::Connection, track: &str) -> String {
    db.query_row(
        "SELECT local_album_id FROM local_tracks WHERE id = ?1",
        [track],
        |row| row.get(0),
    )
    .expect("track row")
}

fn indexed_at(library: &LibrarySetup, relative: &str) -> String {
    library
        .scan_store
        .track_at("music", relative)
        .unwrap()
        .expect("track indexed")
}

/// What people made on one track: plays, stars, playlist entries,
/// bookmarks, saved queue entries and track overrides.
fn holdings(db: &rusqlite::Connection, track: &str) -> [i64; 6] {
    [
        "SELECT COUNT(*) FROM library_play_history WHERE local_track_id = ?1",
        "SELECT COUNT(*) FROM library_user_favorites WHERE item_kind = 'track' AND item_id = ?1",
        "SELECT COUNT(*) FROM playlist_tracks WHERE library_file_id = ?1 \
         AND track_source_id = ?1",
        "SELECT COUNT(*) FROM compat_bookmarks WHERE file_id = ?1",
        "SELECT COUNT(*) FROM compat_play_queue_items WHERE file_id = ?1",
        "SELECT COUNT(*) FROM library_management_overrides WHERE local_track_id = ?1",
    ]
    .map(|sql| {
        db.query_row(sql, [track], |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    })
}

fn continuations(db: &rusqlite::Connection) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'continue_tracks'",
        [],
        |row| row.get(0),
    )
    .expect("actions read")
}

/// The old row's own record: id, path, availability, when it went
/// missing, and its management and identity rows.
type OldRow = (String, String, String, Option<f64>, i64);

fn old_row(db: &rusqlite::Connection, track: &str) -> OldRow {
    db.query_row(
        "SELECT id, relative_path, availability, missing_since, \
         (SELECT COUNT(*) FROM library_track_management_state s WHERE s.local_track_id = t.id) \
         + (SELECT COUNT(*) FROM local_track_external_identities i WHERE i.local_track_id = t.id) \
         FROM local_tracks t WHERE id = ?1",
        [track],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .expect("old row reads")
}

/// An album whose edition a person chose is retagged to a new name with
/// its release ids removed: three files in place, two also renamed. The
/// scan after next joins the album and the app places the new files on
/// the edition; then what people made on each old track is on the track
/// that took its place: plays, stars (a star a person already had on the
/// new track keeps its date, the old one goes), a playlist entry,
/// bookmarks (the later of two stays), played marks (two merge, the later
/// of each time kept), a saved queue in its order and at its place, and
/// track overrides (the new track's own for a field wins).
/// The old rows keep their ids, paths, the day they went missing and
/// their records, and one audit row tells it, with every row it deleted.
/// An album marked played or unplayed afterwards leaves the old rows
/// alone. A second scan moves nothing more; a star and a playlist entry a
/// stale client writes against an old id move at the next scan's end.
#[tokio::test]
async fn a_retagged_and_renamed_album_keeps_what_people_made() {
    let (scratch, library, music, album, tracks) = album_at_rest("lib-continue-chosen", 5).await;
    let db = scratch_db(&scratch);
    db.execute_batch(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'), \
         ('listener', 'Listener', 'user', '2026-01-01T00:00:00Z'); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at, row_revision) VALUES ('{album}', 'musicbrainz', 'rg-1', '{REL_1}', \
         'manual', 'curator', 1234.5, 7);"
    ))
    .expect("edition chosen");
    queue_remap_row(&library, &album, "place_only");
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &album),
    )
    .await;
    for track in &tracks {
        assert_eq!(placement(&db, track).0.as_deref(), Some(REL_1), "placed");
    }
    let (o3, o4, o5) = (&tracks[2], &tracks[3], &tracks[4]);
    db.execute_batch(&format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES \
         ('p1', 'curator', '{o4}', '{album}', 'Four', 'A', '2026-01-01T00:00:00Z'), \
         ('p2', 'curator', '{o4}', '{album}', 'Four', 'A', '2026-01-02T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) VALUES \
         ('curator', 'track', '{o4}', 10), ('listener', 'track', '{o4}', 11); \
         INSERT INTO library_user_favorite_names (user_id, item_kind, item_id, display_name) \
         VALUES ('curator', 'track', '{o4}', 'Four'), ('listener', 'track', '{o4}', 'Four'); \
         INSERT INTO playlists (id, name, created_at, updated_at) VALUES ('list', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, library_file_id, source_type, created_at) \
         VALUES ('entry', 'list', 0, 'Five', 'A', 'B', '{album}', '{o5}', '{o5}', 'local', ''); \
         INSERT INTO compat_bookmarks (user_id, file_id, position_ms, comment, created_at, \
         changed_at) VALUES ('curator', '{o4}', 1000, 'mine', 1, 1), \
         ('listener', '{o4}', 2000, 'later', 1, 30); \
         INSERT INTO compat_play_queues (user_id, current_index, position_ms, updated_at) \
         VALUES ('curator', 1, 500, 1); \
         INSERT INTO compat_play_queue_items (user_id, item_index, file_id) VALUES \
         ('curator', 0, '{o3}'), ('curator', 1, '{o4}'), ('curator', 2, '{o5}'); \
         INSERT INTO library_management_overrides (id, subject_kind, local_track_id, \
         field_name, value_json, mode, subject_revision, created_at, updated_at) VALUES \
         ('ov-title', 'track', '{o4}', 'title', '\"Mine\"', 'replace', 1, 1, 1), \
         ('ov-old', 'track', '{o5}', 'comment', '\"Old\"', 'replace', 1, 1, 1); \
         INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ('curator', '{o4}', 100, NULL), ('listener', '{o4}', 10, 5);"
    ))
    .expect("people made things");

    for number in 1..=3 {
        retag_outside(&music, number, "Renamed", false, false);
    }
    let renamed: Vec<String> = (4..=5)
        .map(|number| retag_outside(&music, number, "Renamed", false, true))
        .collect();
    rescan(&library).await;
    rescan(&library).await;
    let (n4, n5) = (
        indexed_at(&library, &renamed[0]),
        indexed_at(&library, &renamed[1]),
    );
    let joined = album_row(&db, &n4);
    for track in [o4, o5, &n5] {
        assert_eq!(album_row(&db, track), joined, "the album is joined");
    }
    assert_eq!(choice_row(&db, &joined).0, "manual");
    assert_eq!(continuations(&db), 0, "unplaced files carry nothing");
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) VALUES \
         ('listener', 'track', '{n4}', 99); \
         INSERT INTO compat_bookmarks (user_id, file_id, position_ms, comment, created_at, \
         changed_at) VALUES ('listener', '{n4}', 3000, 'earlier', 1, 20); \
         INSERT INTO library_management_overrides (id, subject_kind, local_track_id, \
         field_name, value_json, mode, subject_revision, created_at, updated_at) VALUES \
         ('ov-new', 'track', '{n5}', 'comment', '\"New\"', 'replace', 1, 1, 1); \
         INSERT INTO library_user_play_marks (user_id, local_track_id, marked_played_at, \
         marked_unplayed_at) VALUES ('listener', '{n4}', 60, NULL);"
    ))
    .expect("the listener starred, bookmarked and marked the new track");
    let records = [old_row(&db, o4), old_row(&db, o5)];
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &joined),
    )
    .await;

    for track in [&n4, &n5] {
        assert_eq!(placement(&db, track).0.as_deref(), Some(REL_1), "placed");
    }
    assert_eq!(holdings(&db, &n4), [2, 2, 0, 2, 1, 1]);
    assert_eq!(holdings(&db, &n5), [0, 0, 1, 0, 1, 1]);
    assert_eq!(holdings(&db, o4), [0; 6]);
    assert_eq!(
        holdings(&db, o5),
        [0, 0, 0, 0, 0, 1],
        "the losing override stays"
    );
    assert_eq!(
        holdings(&db, o3)[4],
        1,
        "a track kept in place keeps its own"
    );
    let stars: Vec<(String, f64, Option<String>)> = db
        .prepare(
            "SELECT f.user_id, f.created_at, n.display_name FROM library_user_favorites f \
             LEFT JOIN library_user_favorite_names n ON n.user_id = f.user_id \
             AND n.item_kind = f.item_kind AND n.item_id = f.item_id \
             WHERE f.item_id = ?1 ORDER BY f.user_id",
        )
        .expect("prepare")
        .query_map([&n4], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("stars")
        .collect::<Result<_, _>>()
        .expect("stars read");
    assert_eq!(
        stars,
        vec![
            ("curator".to_owned(), 10.0, Some("Four".to_owned())),
            ("listener".to_owned(), 99.0, Some("Four".to_owned()))
        ]
    );
    let bookmarks: Vec<(String, String)> = db
        .prepare("SELECT user_id, comment FROM compat_bookmarks ORDER BY user_id")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("bookmarks")
        .collect::<Result<_, _>>()
        .expect("bookmarks read");
    assert_eq!(
        bookmarks,
        vec![
            ("curator".to_owned(), "mine".to_owned()),
            ("listener".to_owned(), "later".to_owned())
        ]
    );
    let marks: Vec<(String, String, Option<i64>, Option<i64>)> = db
        .prepare(
            "SELECT user_id, local_track_id, marked_played_at, marked_unplayed_at \
             FROM library_user_play_marks ORDER BY user_id",
        )
        .expect("prepare")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("marks")
        .collect::<Result<_, _>>()
        .expect("marks read");
    assert_eq!(
        marks,
        vec![
            ("curator".to_owned(), n4.clone(), Some(100), None),
            ("listener".to_owned(), n4.clone(), Some(60), Some(5))
        ],
        "marks on both rows merge, the later of each time kept"
    );
    let queue: Vec<String> = db
        .prepare("SELECT file_id FROM compat_play_queue_items ORDER BY item_index")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("queue")
        .collect::<Result<_, _>>()
        .expect("queue read");
    assert_eq!(queue, vec![o3.clone(), n4.clone(), n5.clone()]);
    let current: i64 = db
        .query_row("SELECT current_index FROM compat_play_queues", [], |row| {
            row.get(0)
        })
        .expect("queue index");
    assert_eq!(current, 1);
    let comment: String = db
        .query_row(
            "SELECT value_json FROM library_management_overrides WHERE local_track_id = ?1",
            [&n5],
            |row| row.get(0),
        )
        .expect("override");
    assert_eq!(comment, "\"New\"");
    assert_eq!(
        [old_row(&db, o4), old_row(&db, o5)],
        records,
        "the old rows keep their ids, paths, days and records"
    );
    assert_eq!(continuations(&db), 1);
    let audit: String = db
        .query_row(
            "SELECT after_json FROM library_catalog_actions WHERE action_kind = 'continue_tracks'",
            [],
            |row| row.get(0),
        )
        .expect("audit row");
    let audit: Value = serde_json::from_str(&audit).expect("audit json");
    assert_eq!(audit["new_pairs"], json!(2), "{audit}");
    let pair = audit["pairs"]
        .as_array()
        .expect("pairs")
        .iter()
        .find(|pair| pair["old_track_id"] == json!(o4))
        .expect("pair of track four")
        .clone();
    assert_eq!(pair["new_track_id"], json!(n4), "{audit}");
    assert_eq!(
        pair["moved"]["play_history_ids"],
        json!(["p1", "p2"]),
        "{audit}"
    );
    assert_eq!(
        pair["deleted"]["stars"][0]["created_at"],
        json!(11.0),
        "{audit}"
    );
    assert_eq!(
        pair["deleted"]["stars"][0]["display_name"],
        json!("Four"),
        "{audit}"
    );
    assert_eq!(
        pair["deleted"]["bookmarks"][0]["comment"],
        json!("earlier"),
        "{audit}"
    );
    assert_eq!(
        pair["moved"]["star_names_copied"],
        json!([{"user_id": "listener", "display_name": "Four"}]),
        "{audit}"
    );
    assert_eq!(
        pair["moved"]["overrides"],
        json!([{"id": "ov-title", "updated_at": 1.0, "row_revision": 1}]),
        "{audit}"
    );
    assert_eq!(
        pair["moved"]["play_mark_user_ids"],
        json!(["curator"]),
        "{audit}"
    );
    assert_eq!(
        pair["moved"]["play_marks_merged_into"],
        json!([{"user_id": "listener", "local_track_id": n4, "marked_played_at": 60,
                "marked_unplayed_at": null}]),
        "{audit}"
    );
    assert_eq!(
        pair["deleted"]["play_marks"],
        json!([{"user_id": "listener", "local_track_id": o4, "marked_played_at": 10,
                "marked_unplayed_at": 5}]),
        "{audit}"
    );
    assert_eq!(
        audit["moved"]["library_user_play_marks"],
        json!(2),
        "{audit}"
    );

    let titles: Vec<String> = db
        .prepare(
            "SELECT title FROM local_tracks WHERE id IN (?1, ?2) \
             ORDER BY disc_number, track_number",
        )
        .expect("prepare")
        .query_map([&n4, &n5], |row| row.get(0))
        .expect("titles")
        .collect::<Result<_, _>>()
        .expect("titles read");
    // Newest first: the move, then the scan's join of the retagged half
    // and its first half left as it was.
    let history = album_history(&library, &joined, "listener");
    assert_eq!(
        history["items"][0],
        json!({"kind": "tracks_continued", "at": history["items"][0]["at"], "by": null,
               "track_count": 2, "track_titles": titles, "other_album_title": null,
               "undo_id": null}),
        "{history}"
    );
    assert_eq!(
        history_kinds(&history),
        ["tracks_continued", "retag_joined", "retag_half_left_alone"],
        "{history}"
    );
    assert!(history["items"][0]["at"].as_i64().is_some(), "{history}");
    let sent = history.to_string();
    for private in [
        "curator", "listener", "Curator", "Listener", "mine", "later", "earlier", "p1", o4, o5,
        &n4, &n5, &album, &joined,
    ] {
        assert!(!sent.contains(private), "{private} in {sent}");
    }

    // An album marked from a player app marks the tracks that answer for
    // it, never an old row that continued into one.
    let path = scratch.join("app.db");
    PlayMarkStore::new(CollectionsDb::new(
        droppedneedle_kernel::db::Db::open_for_tests(&path).expect("database opens"),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ))
    .mark("listener", MarkScope::Album(&joined), false)
    .await
    .expect("the album is marked");
    let on_old_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_user_play_marks WHERE local_track_id IN (?1, ?2)",
            [o4, o5],
            |row| row.get(0),
        )
        .expect("marks count");
    assert_eq!(on_old_rows, 0, "an album mark skips the continued rows");

    rescan(&library).await;
    assert_eq!(continuations(&db), 1, "a second scan moves nothing");

    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) VALUES \
         ('stale', 'track', '{o5}', 5); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, track_source_id, library_file_id, source_type, created_at) \
         VALUES ('late', 'list', 1, 'Five', 'A', 'B', '{o5}', '{o5}', 'local', '');"
    ))
    .expect("a stale client wrote against an old id");
    rescan(&library).await;
    assert_eq!(holdings(&db, o5)[..3], [0, 0, 0]);
    assert_eq!(holdings(&db, &n5)[1..3], [1, 2]);
    assert_eq!(
        history_kinds(&album_history(&library, &joined, "listener")),
        ["tracks_continued", "retag_joined", "retag_half_left_alone"],
        "a sweep adds no entry"
    );
}

/// A track nobody played but people rated continues into the track that
/// took its place with its ratings: a rating the user already gave the
/// new track stays, the old one goes, and the audit row lists both.
#[tokio::test]
async fn a_rated_track_continues_with_its_ratings() {
    let (scratch, library, music, album, tracks) = album_at_rest("lib-continue-rated", 3).await;
    let db = scratch_db(&scratch);
    db.execute_batch(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'), \
         ('listener', 'Listener', 'user', '2026-01-01T00:00:00Z'); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at, row_revision) VALUES ('{album}', 'musicbrainz', 'rg-1', '{REL_1}', \
         'manual', 'curator', 1234.5, 7);"
    ))
    .expect("edition chosen");
    queue_remap_row(&library, &album, "place_only");
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &album),
    )
    .await;
    let old = &tracks[2];
    db.execute_batch(&format!(
        "INSERT INTO library_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
         VALUES ('listener', 'track', '{old}', 4, 1000), ('curator', 'track', '{old}', 2, 2000);"
    ))
    .expect("rated");

    for number in 1..=2 {
        retag_outside(&music, number, "Renamed", false, false);
    }
    let renamed = retag_outside(&music, 3, "Renamed", false, true);
    rescan(&library).await;
    rescan(&library).await;
    let new = indexed_at(&library, &renamed);
    let joined = album_row(&db, &new);
    assert_eq!(album_row(&db, old), joined, "the album is joined");
    db.execute(
        "INSERT INTO library_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
         VALUES ('curator', 'track', ?1, 5, 3000)",
        [&new],
    )
    .expect("the curator rated the new track");
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &joined),
    )
    .await;

    let ratings = |track: &str| -> Vec<(String, i64, i64)> {
        db.prepare(
            "SELECT user_id, rating, rated_at FROM library_user_ratings \
             WHERE item_kind = 'track' AND item_id = ?1 ORDER BY user_id",
        )
        .expect("prepare")
        .query_map([track], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("ratings")
        .collect::<Result<_, _>>()
        .expect("ratings read")
    };
    assert_eq!(
        ratings(&new),
        vec![
            ("curator".to_owned(), 5, 3000),
            ("listener".to_owned(), 4, 1000)
        ]
    );
    assert!(ratings(old).is_empty());
    let audit: String = db
        .query_row(
            "SELECT after_json FROM library_catalog_actions WHERE action_kind = 'continue_tracks'",
            [],
            |row| row.get(0),
        )
        .expect("audit row");
    let audit: Value = serde_json::from_str(&audit).expect("audit json");
    let pair = audit["pairs"]
        .as_array()
        .expect("pairs")
        .iter()
        .find(|pair| pair["old_track_id"] == json!(old))
        .expect("pair of the rated track")
        .clone();
    assert_eq!(pair["new_track_id"], json!(new), "{audit}");
    assert_eq!(
        pair["moved"]["rating_user_ids"],
        json!(["listener"]),
        "{audit}"
    );
    assert_eq!(
        pair["deleted"]["ratings"],
        json!([{"user_id": "curator", "rating": 2, "rated_at": 2000}]),
        "{audit}"
    );
    assert_eq!(audit["moved"]["library_user_ratings"], json!(2), "{audit}");
}

fn history_kinds(history: &Value) -> Vec<String> {
    history["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["kind"].as_str().expect("kind").to_owned())
        .collect()
}

/// An album's history as the route sends it to `caller`.
fn album_history(library: &LibrarySetup, album: &str, caller: &str) -> Value {
    use droppedneedle_library::http::history::album_history_view;
    let view = album_history_view(&crate::common::library::operations(library), album, caller)
        .expect("history reads");
    serde_json::to_value(view).expect("history serialises")
}

/// An album the app chose by itself, retagged to a new name with its
/// release kept and renamed: the new files land beside the old ones, one
/// identification pass maps them, and their history is on them without
/// another scan.
#[tokio::test]
async fn an_automatic_album_retagged_and_renamed_keeps_its_history() {
    let (scratch, library, music, album, old) = album_at_rest("lib-continue-automatic", 3).await;
    let db = scratch_db(&scratch);
    let providers = &scratch.identify;
    providers.set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    assert_eq!(choice_row(&db, &album).0, "automatic");
    assert_eq!(placement(&db, &old[0]).0.as_deref(), Some(REL_1));
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        old[0]
    ))
    .expect("starred");

    let renamed: Vec<String> = (1..=3)
        .map(|number| retag_outside(&music, number, "Renamed", true, true))
        .collect();
    rescan(&library).await;
    let new = indexed_at(&library, &renamed[0]);
    assert_eq!(
        library.scan_store.album_for_track(&new).unwrap().as_deref(),
        Some(album.as_str())
    );
    assert_eq!(
        holdings(&db, &new)[1],
        0,
        "nothing before the files are mapped"
    );
    providers.set_recall(fitting_release(&library, &album));
    library.identification.identify_tick().await;
    assert_eq!(holdings(&db, &new)[1], 1);
    assert_eq!(holdings(&db, &old[0])[1], 0);
}

/// An album whose `count` tracks the app identified by itself, the first
/// starred, retagged outside the app to a new name with its MusicBrainz
/// ids removed and renamed, and scanned: the new files make an album row
/// of their own. Returns the scratch folder, the library, the old album
/// and its rows, and the new album and its rows.
async fn retagged_without_ids(
    tag: &str,
) -> (
    Scratch,
    LibrarySetup,
    String,
    Vec<String>,
    String,
    Vec<String>,
) {
    let (scratch, library, music, album, old) = album_at_rest(tag, 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    db.execute_batch(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        old[0]
    ))
    .expect("starred");
    let renamed: Vec<String> = (1..=3)
        .map(|number| retag_outside(&music, number, "Renamed", false, true))
        .collect();
    rescan(&library).await;
    let new: Vec<String> = renamed
        .iter()
        .map(|path| indexed_at(&library, path))
        .collect();
    let own_row = library
        .scan_store
        .album_for_track(&new[0])
        .unwrap()
        .expect("album");
    assert_ne!(own_row, album, "an album row of its own");
    (scratch, library, album, old, own_row, new)
}

fn retired_into(db: &rusqlite::Connection, album: &str) -> Option<String> {
    db.query_row(
        "SELECT retired_into_album_id FROM local_albums WHERE id = ?1",
        [album],
        |row| row.get(0),
    )
    .expect("album row")
}

/// The new album identified as a best guess, or as another edition, is
/// not joined to the album it replaced. A person approving the guess, or
/// choosing the old album's edition for it, joins them in that request,
/// and so does the app placing the files on an edition a person chose.
/// The old album's "leave unmanaged" goes, as the new album holds a
/// person's match.
#[tokio::test]
async fn a_persons_match_joins_an_album_retagged_without_its_ids() {
    use droppedneedle_library::identify::models::{
        CandidateEvidence, EvidenceClass, TrackEvidence,
    };

    // A best guess, approved.
    let (scratch, library, album, old, own_row, new) =
        retagged_without_ids("lib-join-approve").await;
    let db = scratch_db(&scratch);
    let providers = &scratch.identify;
    let mut tied = fitting_release(&library, &own_row);
    let mut sibling = tied.releases[0].clone();
    sibling.id = REL_2.to_owned();
    sibling.release_group_id = "rg-2".to_owned();
    tied.releases.push(sibling);
    providers.set_recall(tied);
    library.identification.identify_tick().await;
    assert!(match_state(&db, &own_row).is_some(), "a best guess");
    assert_eq!(retired_into(&db, &album), None);
    exclude_by_hand(&db, &album);
    let key = format!("rg-1:{REL_1}");
    droppedneedle_library::identify::review::file_review(
        &library.identify_store,
        "rev-join",
        &own_row,
        "EDITION_UNCERTAIN",
        vec![CandidateEvidence {
            candidate_key: key.clone(),
            release_group_mbid: "rg-1".to_owned(),
            release_mbid: Some(REL_1.to_owned()),
            album_title: "Renamed".to_owned(),
            album_artist_name: String::new(),
            track_evidence: new
                .iter()
                .enumerate()
                .map(|(index, track)| TrackEvidence {
                    local_track_id: track.clone(),
                    classification: EvidenceClass::Supported,
                    evidence_kinds: Vec::new(),
                    recording_mbid: Some(format!("rec-{}", index + 1)),
                    release_track_mbid: Some(format!("rt-{}", index + 1)),
                })
                .collect(),
            score: 1.0,
            reason_code: "EDITION_UNCERTAIN".to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        }],
    )
    .unwrap();
    library
        .identification
        .approve_review("rev-join", "curator", &key)
        .expect("approved");
    assert_eq!(retired_into(&db, &album), Some(own_row.clone()));
    assert_eq!(
        (holdings(&db, &old[0])[1], holdings(&db, &new[0])[1]),
        (0, 1)
    );
    let held: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_management_exclusions \
             WHERE local_album_id IN (?1, ?2)",
            [&album, &own_row],
            |row| row.get(0),
        )
        .expect("exclusions read");
    assert_eq!(held, 0, "the new album's person's match stands");

    // Another edition, then the old album's edition chosen for it.
    let (scratch, library, album, old, own_row, new) =
        retagged_without_ids("lib-join-choose").await;
    let db = scratch_db(&scratch);
    let providers = &scratch.identify;
    let mut both = fitting_release(&library, &own_row);
    let mut sibling = both.releases[0].clone();
    sibling.id = REL_2.to_owned();
    sibling.release_group_id = "rg-2".to_owned();
    providers.set_recall(RecallResult {
        releases: vec![sibling.clone()],
        ..RecallResult::default()
    });
    library.identification.identify_tick().await;
    let (source, release, ..) = choice_row(&db, &own_row);
    assert_eq!(
        (source.as_str(), release.as_deref()),
        ("automatic", Some(REL_2))
    );
    assert_eq!(retired_into(&db, &album), None);
    both.releases.push(sibling);
    providers.set_recall(both);
    crate::common::library::operations(&library)
        .choose_edition(&own_row, REL_1, Some("curator"))
        .await
        .expect("edition chosen");
    assert_eq!(retired_into(&db, &album), Some(own_row.clone()));
    assert_eq!(
        (holdings(&db, &old[0])[1], holdings(&db, &new[0])[1]),
        (0, 1)
    );

    // A person's edition placed on the files later.
    let (scratch, library, album, old, own_row, new) = retagged_without_ids("lib-join-remap").await;
    let db = scratch_db(&scratch);
    db.execute_batch(&format!(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at) VALUES ('{own_row}', 'musicbrainz', 'rg-1', '{REL_1}', 'manual', \
         'curator', 1);"
    ))
    .expect("edition chosen");
    queue_remap_row(&library, &own_row, "place_only");
    assert_eq!(retired_into(&db, &album), None);
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &own_row),
    )
    .await;
    assert_eq!(retired_into(&db, &album), Some(own_row));
    assert_eq!(
        (holdings(&db, &old[0])[1], holdings(&db, &new[0])[1]),
        (0, 1)
    );
}

impl ManagedLibrary {
    /// Retag every one of `tracks` outside the app to the album "Retagged"
    /// with its MusicBrainz ids removed, rename each into `retagged/`, and
    /// scan. Returns the new rows, in order.
    async fn retag_without_ids(&self, tracks: &[String]) -> Vec<String> {
        let moved = self.retag_into(tracks, "Retagged", "retagged");
        rescan(&self.library).await;
        moved
            .iter()
            .map(|path| indexed_at(&self.library, path))
            .collect()
    }

    /// MusicBrainz offering `album`'s files as the one edition `like` is
    /// matched to, each track under the release track id of `like`'s track
    /// of the same title, and identification run until it is idle.
    async fn identify_as(&self, album: &str, like: &str) {
        let db = self.db();
        let (release, group): (String, String) = db
            .query_row(
                "SELECT release_mbid, release_group_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1",
                [like],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("edition");
        let mut recall = fitting_release(&self.library, album);
        let edition = &mut recall.releases[0];
        edition.id = release;
        edition.release_group_id = group;
        for track in &mut edition.tracks {
            (track.id, track.recording_id) = db
                .query_row(
                    "SELECT i.release_track_mbid, i.recording_mbid FROM local_tracks t \
                     JOIN local_track_external_identities i ON i.local_track_id = t.id \
                     WHERE t.local_album_id = ?1 AND t.title = ?2",
                    [like, &track.title],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("the old album's track");
        }
        self.scratch.identify.set_recall(recall);
        while self.library.identification.identify_tick().await > 0 {}
    }
}

/// The row's path and when it went missing.
fn where_gone(db: &rusqlite::Connection, track: &str) -> (String, String, Option<f64>) {
    db.query_row(
        "SELECT relative_path, availability, missing_since FROM local_tracks WHERE id = ?1",
        [track],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("track row")
}

/// An album the app identified and wrote by itself, retagged outside the
/// app to a new name with its MusicBrainz ids removed and renamed in one
/// go: the files come back as a new album, and the identification pass
/// that matches it to the same edition joins the two. The new album keeps
/// its id and name, the old one opens it, its rows sit on it missing as
/// they were, and what people made moves to the new tracks with no second
/// scan. Automatic management then finds the files changed outside the
/// app and writes nothing.
#[tokio::test]
async fn an_automatic_album_retagged_without_its_ids_is_joined_at_the_seal() {
    let managed = ManagedLibrary::start("lib-join-seal", |_| {}).await;
    let (album, old) = managed.add_album("first", 2).await;
    let db = managed.db();
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET decision_source = 'automatic', \
         selected_by_user_id = NULL WHERE local_album_id = '{album}'; \
         UPDATE local_track_external_identities SET decision_source = 'automatic' \
         WHERE local_track_id IN ('{}', '{}');",
        old[0], old[1]
    ))
    .expect("the app's own match");
    managed.activate().await;
    managed.look(&album).await;
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert!(
        managed.recorded(&old[0]).is_some(),
        "the app wrote the files"
    );
    db.execute_batch(&format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES ('p1', 'listener', '{o1}', '{album}', \
         'T', 'A', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{o1}', 1), ('listener', 'album', '{album}', 1); \
         INSERT INTO playlists (id, name, created_at, updated_at) \
         VALUES ('list', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, library_file_id, source_type, created_at) \
         VALUES ('entry', 'list', 0, 'T', 'A', 'B', '{album}', '{o2}', '{o2}', 'local', '');",
        o1 = old[0],
        o2 = old[1]
    ))
    .expect("a listener's history");

    let new = managed.retag_without_ids(&old).await;
    let own_row = managed.album_of(&new[0]);
    assert_ne!(own_row, album, "an album row of its own");
    let gone: Vec<_> = old.iter().map(|track| where_gone(&db, track)).collect();
    let bytes: Vec<String> = new
        .iter()
        .map(|track| file_sha256(&managed.path(track)))
        .collect();
    managed.identify_as(&own_row, &album).await;

    assert_eq!(retired_into(&db, &album), Some(own_row.clone()));
    let title: String = db
        .query_row(
            "SELECT title FROM local_albums WHERE id = ?1",
            [&own_row],
            |row| row.get(0),
        )
        .expect("album");
    assert_eq!(title, "Retagged");
    let opens: String = db
        .query_row(
            "SELECT local_album_id FROM local_album_aliases WHERE alias = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("the old id is an alias");
    assert_eq!(opens, own_row);
    for (track, was) in old.iter().zip(&gone) {
        assert_eq!(managed.album_of(track), own_row);
        assert_eq!(&where_gone(&db, track), was, "kept as it was");
    }
    assert_eq!(holdings(&db, &new[0])[..2], [1, 1]);
    assert_eq!(holdings(&db, &new[1])[2], 1);
    assert_eq!(holdings(&db, &old[0])[..2], [0, 0]);
    let album_star: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_user_favorites WHERE item_kind = 'album' \
             AND item_id = ?1",
            [&own_row],
            |row| row.get(0),
        )
        .expect("stars read");
    assert_eq!(album_star, 1);
    let joins: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = 'merge_album' \
             AND reason_code = 'RETAG_JOINED_SAME_RELEASE'",
            [],
            |row| row.get(0),
        )
        .expect("actions read");
    assert_eq!((joins, continuations(&db)), (1, 1));
    let waiting: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_identify_jobs WHERE local_album_id = ?1 \
             AND state IN ('queued', 'running', 'deferred')",
            [&own_row],
            |row| row.get(0),
        )
        .expect("jobs read");
    assert_eq!(waiting, 0, "not offered again");

    managed.look(&own_row).await;
    assert_eq!(managed.mark(&own_row).as_deref(), Some("files_changed"));
    let after: Vec<String> = new
        .iter()
        .map(|track| file_sha256(&managed.path(track)))
        .collect();
    assert_eq!(after, bytes, "nothing written");

    rescan(&managed.library).await;
    let actions = |kind: &str| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = ?1",
            [kind],
            |row| row.get(0),
        )
        .expect("actions read")
    };
    assert_eq!(
        (actions("merge_album"), actions("continue_tracks")),
        (1, 1),
        "a second scan writes nothing more"
    );
}

impl ManagedLibrary {
    /// Retag every one of `tracks` outside the app to the album `title`
    /// with its MusicBrainz ids removed, and rename each into `folder`.
    /// Returns the new paths, in order. Nothing is scanned.
    fn retag_into(&self, tracks: &[String], title: &str, folder: &str) -> Vec<String> {
        use droppedneedle_library::tags::{TagEdit, TagField, save_tags};
        let mut moved = Vec::new();
        for (index, track) in tracks.iter().enumerate() {
            let from = self.path(track);
            let mut edits = vec![TagEdit::verbatim(TagField::Album, vec![title.to_owned()])];
            edits.extend(
                [
                    TagField::MusicBrainzReleaseId,
                    TagField::MusicBrainzReleaseGroupId,
                    TagField::MusicBrainzReleaseTrackId,
                    TagField::MusicBrainzRecordingId,
                ]
                .map(|field| TagEdit::verbatim(field, Vec::new())),
            );
            save_tags(&from, &edits).expect("retagged outside the app");
            let to = format!("{folder}/{:02}.flac", index + 1);
            let dest = self.music.join(&to);
            std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
            std::fs::rename(&from, &dest).expect("renamed outside the app");
            moved.push(to);
        }
        moved
    }

    /// The skip codes automatic management lists, by album.
    fn skipped_codes(&self) -> Vec<(String, String)> {
        self.management()
            .automatic_skipped(50)
            .expect("skipped")
            .into_iter()
            .map(|album| (album.album_id, album.reason.code))
            .collect()
    }

    /// How many runs were made for the album.
    fn runs_for(&self, album: &str) -> i64 {
        self.db()
            .query_row(
                "SELECT COUNT(*) FROM library_management_job_snapshots s \
                 WHERE EXISTS (SELECT 1 FROM json_each(s.selection_json, '$.ids') \
                   WHERE value = ?1)",
                [album],
                |row| row.get(0),
            )
            .expect("runs read")
    }

    /// Folders a scan holds back.
    fn held_scopes(&self) -> Vec<(String, String, i64)> {
        self.db()
            .prepare(
                "SELECT root_id, relative_path, held_at FROM library_scan_held_scopes \
                 ORDER BY root_id, relative_path",
            )
            .expect("held scopes")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("held scopes")
            .collect::<rusqlite::Result<_>>()
            .expect("held scopes")
    }

    /// Whether the latest scan held the whole music folder back.
    fn latest_scan_held_back(&self) -> bool {
        self.count(
            "SELECT COUNT(*) FROM library_scan_failures WHERE run_id = \
             (SELECT id FROM library_scan_runs ORDER BY started_at DESC LIMIT 1) \
             AND root_id = 'music' AND relative_path = '.' \
             AND failure_code = 'MASS_MISSING_GUARD'",
        ) > 0
    }
}

/// An album the app wrote, retagged and renamed outside it in a pass that
/// also moves a file out of the folder for a while: the scan holds the
/// folder back, so the old rows stay and nothing can tell the new album
/// replaced them. Automatic management writes none of the new files and
/// says why. The next scan finds the folder as expected and marks the old
/// rows missing: at its end the new albums, matched to the old albums'
/// editions, are left unmanaged as the person's retag. The join waits a
/// scan, and so does the hold. The scan after joins the old albums into the
/// new ones (one of them lost an extra file in the retag), the hold lifts,
/// and the new albums stay unmanaged.
#[tokio::test]
async fn new_files_wait_while_a_scan_holds_their_folder_back() {
    let managed = ManagedLibrary::start("lib-held-scope", |_| {}).await;
    let (album, old) = managed.add_album("first", 2).await;
    let (album_two, old_two) = managed.add_album("second", 3).await;
    let (_, others) = managed.add_album("third", 11).await;
    let db = managed.db();
    for (id, tracks) in [(&album, &old), (&album_two, &old_two)] {
        db.execute(
            "UPDATE local_album_external_identities SET decision_source = 'automatic', \
             selected_by_user_id = NULL WHERE local_album_id = ?1",
            rusqlite::params![id],
        )
        .expect("the app's own match");
        for track in tracks.iter() {
            db.execute(
                "UPDATE local_track_external_identities SET decision_source = 'automatic' \
                 WHERE local_track_id = ?1",
                [track],
            )
            .expect("the app's own match");
        }
    }
    managed.activate().await;
    managed.look(&album).await;
    managed.look(&album_two).await;
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert!(
        managed.recorded(&old[0]).is_some(),
        "the app wrote the files"
    );
    assert!(managed.recorded(&old_two[0]).is_some());

    // One pass of a tagger: both albums retagged and renamed, the second
    // one's extra file deleted, and another album's files still away.
    let paths = managed.retag_into(&old, "Retagged", "new");
    let paths_two = managed.retag_into(&old_two[..2], "Retagged Two", "new-two");
    std::fs::remove_file(managed.path(&old_two[2])).expect("extra deleted");
    let away = managed.scratch.to_path_buf().join("away");
    std::fs::create_dir_all(&away).expect("away");
    let moved: Vec<(PathBuf, PathBuf)> = others
        .iter()
        .enumerate()
        .map(|(index, track)| (managed.path(track), away.join(format!("{index}.flac"))))
        .collect();
    for (from, to) in &moved {
        std::fs::rename(from, to).expect("moved away");
    }
    rescan(&managed.library).await;
    assert!(managed.latest_scan_held_back(), "16 gone of 20");
    let indexed = |album: &str| {
        db.query_row(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1 \
             AND availability = 'indexed'",
            [album],
            |row| row.get::<_, i64>(0),
        )
        .expect("rows read")
    };
    assert_eq!(indexed(&album), 2, "nothing marked missing");
    let new: Vec<String> = paths
        .iter()
        .map(|path| indexed_at(&managed.library, path))
        .collect();
    let new_two: Vec<String> = paths_two
        .iter()
        .map(|path| indexed_at(&managed.library, path))
        .collect();
    let own_row = managed.album_of(&new[0]);
    let own_row_two = managed.album_of(&new_two[0]);
    assert_ne!(own_row, album);
    let defer_two = |not_before: i64| {
        db.execute(
            "UPDATE library_identify_jobs SET not_before_ms = ?2 WHERE local_album_id = ?1 \
             AND state IN ('queued', 'deferred')",
            rusqlite::params![own_row_two, not_before],
        )
        .expect("job deferred")
    };
    defer_two(i64::MAX);
    managed.identify_as(&own_row, &album).await;
    defer_two(0);
    managed.identify_as(&own_row_two, &album_two).await;
    assert_eq!(retired_into(&db, &album), None, "the old rows still stand");
    let bytes = |tracks: &[String]| -> Vec<String> {
        tracks
            .iter()
            .map(|track| file_sha256(&managed.path(track)))
            .collect()
    };
    let (before, before_two) = (bytes(&new), bytes(&new_two));

    managed.look(&own_row).await;
    managed.look(&own_row_two).await;
    assert_eq!(
        (managed.runs_for(&own_row), managed.runs_for(&own_row_two)),
        (0, 0),
        "no run started while held"
    );
    assert_eq!(bytes(&new), before, "nothing written while held");
    assert_eq!(bytes(&new_two), before_two);
    assert_eq!(managed.held_scopes().len(), 1);
    let skipped = managed.management().automatic_skipped(50).expect("skipped");
    let held = skipped
        .iter()
        .find(|skipped| skipped.album_id == own_row)
        .expect("listed");
    let reason = droppedneedle_library::management::automatic::skip_reason("SCAN_HELD_BACK");
    assert_eq!(
        (held.reason.code.as_str(), held.reason.message.as_str()),
        ("SCAN_HELD_BACK", reason.message.as_str())
    );

    // The files come back: the scan walks the folder as expected and
    // marks the old rows missing. The new rows were there before that scan
    // began, so the join waits a scan, and the hold waits for the join.
    for (to, from) in &moved {
        std::fs::rename(from, to).expect("moved back");
    }
    rescan(&managed.library).await;
    assert!(!managed.latest_scan_held_back());
    assert_eq!(indexed(&album), 0, "the old rows are missing");
    assert_eq!(retired_into(&db, &album), None);
    assert_eq!(retired_into(&db, &album_two), None);
    assert_eq!(managed.held_scopes().len(), 1, "held until the join");
    managed.look(&own_row).await;
    managed.look(&own_row_two).await;
    let codes = managed.skipped_codes();
    for new_album in [&own_row, &own_row_two] {
        assert_eq!(managed.mark(new_album).as_deref(), Some("files_changed"));
        assert!(codes.contains(&(new_album.clone(), "TAKEN_OVER".to_owned())));
    }
    assert_eq!(bytes(&new), before);
    assert_eq!(bytes(&new_two), before_two);

    // One more scan joins both albums and lets the folder go.
    rescan(&managed.library).await;
    assert_eq!(retired_into(&db, &album), Some(own_row.clone()));
    assert_eq!(retired_into(&db, &album_two), Some(own_row_two.clone()));
    assert_eq!(managed.held_scopes(), Vec::new());
    managed.worker_looks().await;
    assert_eq!(managed.mark(&own_row).as_deref(), Some("files_changed"));
    assert_eq!(managed.mark(&own_row_two).as_deref(), Some("files_changed"));
    assert_eq!(bytes(&new), before);
    assert_eq!(bytes(&new_two), before_two);
    assert!(
        managed
            .skipped_codes()
            .iter()
            .all(|(_, code)| code != "SCAN_HELD_BACK")
    );
}

/// Who a held folder holds: an album with a scanned file first indexed in
/// it since the hold began. Files from before the hold, downloads, other
/// folders and other roots are looked at as before. An album also taken
/// over says so first, and a held album is still one the app writes by
/// itself, just not now.
#[tokio::test]
async fn a_held_folder_holds_only_files_scanned_into_it_since() {
    let managed = ManagedLibrary::start("lib-held-who", |_| {}).await;
    let (album, tracks) = managed.add_album("p", 2).await;
    managed.activate().await;
    managed.set_automatic(true, true);
    managed.look(&album).await;
    assert!(managed.recorded(&tracks[0]).is_some(), "written");
    let relative = managed.relative(&tracks[0]);
    let (folder, _) = relative.rsplit_once('/').expect("in a folder");
    let db = managed.db();
    let look = |held: &[(&str, &str)], imported_at: f64, source: &str| {
        db.execute_batch(
            "DELETE FROM library_scan_held_scopes; \
             DELETE FROM library_management_automatic_queue;",
        )
        .expect("reset");
        for (root, path) in held {
            db.execute(
                "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
                 VALUES (?1, ?2, 2000000)",
                rusqlite::params![root, path],
            )
            .expect("held");
        }
        db.execute(
            "UPDATE local_tracks SET imported_at = ?2, ingest_source = ?3 \
             WHERE local_album_id = ?1",
            rusqlite::params![album, imported_at, source],
        )
        .expect("rows set");
        managed
            .library
            .identification
            .queue_automatic_management(&album);
        assert!(managed.management().automatic_next().expect("looked"));
        db.query_row(
            "SELECT last_reason_code FROM library_management_automatic_queue \
             WHERE local_album_id = ?1",
            [&album],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or(None)
    };
    let held = Some("SCAN_HELD_BACK".to_owned());
    for (case, scopes, imported_at, source, expected) in [
        (
            "whole root, after",
            &[("music", ".")][..],
            3000.0,
            "scan",
            &held,
        ),
        (
            "its folder, after",
            &[("music", folder)][..],
            3000.0,
            "scan",
            &held,
        ),
        (
            "whole root, before",
            &[("music", ".")][..],
            1000.0,
            "scan",
            &None,
        ),
        (
            "its folder, before",
            &[("music", folder)][..],
            1000.0,
            "scan",
            &None,
        ),
        (
            "a download",
            &[("music", ".")][..],
            3000.0,
            "download",
            &None,
        ),
        (
            "another folder",
            &[("music", "other")][..],
            3000.0,
            "scan",
            &None,
        ),
        (
            "another root",
            &[("elsewhere", ".")][..],
            3000.0,
            "scan",
            &None,
        ),
    ] {
        assert_eq!(&look(scopes, imported_at, source), expected, "{case}");
    }

    db.execute(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, 'files_changed', 1)",
        [&album],
    )
    .expect("taken over");
    assert_eq!(
        look(&[("music", ".")], 3000.0, "scan").as_deref(),
        Some("TAKEN_OVER")
    );
    db.execute(
        "DELETE FROM library_album_takeovers WHERE local_album_id = ?1",
        [&album],
    )
    .expect("handed back");
    assert_eq!(look(&[("music", ".")], 3000.0, "scan"), held);
    let writes = managed
        .management()
        .writes_by_itself(&album)
        .expect("writes read");
    assert!(writes.by_itself && !writes.pending, "{writes:?}");
}

impl ManagedLibrary {
    /// Make the album's match the app's own, as automatic identification
    /// leaves it.
    fn match_by_the_app(&self, album: &str) {
        let db = self.db();
        db.execute(
            "UPDATE local_album_external_identities SET decision_source = 'automatic', \
             selected_by_user_id = NULL WHERE local_album_id = ?1",
            [album],
        )
        .expect("the app's own match");
        db.execute(
            "UPDATE local_track_external_identities SET decision_source = 'automatic' \
             WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            [album],
        )
        .expect("the app's own match");
    }
}

/// An album the app wrote, retagged and renamed outside it with one file
/// deleted for good: every scan finds most of the folder gone, so the
/// guard trips at every scan and the old rows stay. The scan after the new
/// album is matched to the old album's edition leaves it unmanaged as the
/// person's retag, without waiting for the folder. Removing the old album,
/// as the scan advises, lets the folder go and writes nothing; "Manage it
/// again" then writes the new album.
#[tokio::test]
async fn a_retag_that_deleted_a_file_is_left_alone_while_its_folder_stays_held() {
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-held-gone", |_| {}).await;
    let (album, old) = managed.add_album("first", 12).await;
    managed.match_by_the_app(&album);
    managed.activate().await;
    managed.look(&album).await;
    assert!(
        managed.recorded(&old[0]).is_some(),
        "the app wrote the files"
    );

    let mut paths = managed.retag_into(&old, "Retagged", "new");
    let deleted = paths.pop().expect("twelve files");
    std::fs::remove_file(managed.music.join(deleted)).expect("deleted for good");
    rescan(&managed.library).await;
    assert!(managed.latest_scan_held_back(), "12 gone of 23");
    let new: Vec<String> = paths
        .iter()
        .map(|path| indexed_at(&managed.library, path))
        .collect();
    let replacement = managed.album_of(&new[0]);
    assert_ne!(replacement, album);
    // The match is a weak one, so a person confirms it.
    managed.identify_as(&replacement, &album).await;
    crate::common::library::operations(&managed.library)
        .confirm_match(&replacement, "admin")
        .expect("match confirmed");
    managed.look(&replacement).await;
    assert!(
        managed
            .skipped_codes()
            .contains(&(replacement.clone(), "SCAN_HELD_BACK".to_owned()))
    );
    let before = managed.bytes(&new);
    let held = managed.held_scopes();
    assert_eq!(held.len(), 1);
    let indexed = || {
        managed.count(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{album}' \
             AND availability = 'indexed'"
        ))
    };

    // Nothing changed: the guard trips again, and the new album is judged
    // at this run's end.
    rescan(&managed.library).await;
    assert!(managed.latest_scan_held_back(), "still 12 gone of 23");
    assert_eq!(
        managed.mark(&replacement).as_deref(),
        Some("files_changed"),
        "left to the person at the run end"
    );
    assert_eq!(indexed(), 12, "nothing marked missing");
    assert_eq!(managed.held_scopes(), held, "the hold keeps its start");
    managed.worker_looks().await;
    assert_eq!(managed.runs_for(&replacement), 0);
    assert_eq!(managed.bytes(&new), before, "nothing written");

    // The scan's advice: remove the old album. The next scan is clean and
    // lets the folder go, and the new album stays the person's.
    Mutations::new(&managed.library)
        .remove_album(&album, false, true, "admin")
        .expect("removed in the app");
    rescan(&managed.library).await;
    assert!(!managed.latest_scan_held_back());
    assert_eq!(managed.held_scopes(), Vec::new());
    assert_eq!(managed.mark(&replacement).as_deref(), Some("files_changed"));
    managed.worker_looks().await;
    assert_eq!(managed.runs_for(&replacement), 0);
    assert_eq!(managed.bytes(&new), before, "still nothing written");

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&replacement, "admin")
            .expect("handed back")
    );
    managed.worker_looks().await;
    assert_eq!(managed.mark(&replacement), None);
    assert_eq!(managed.runs_for(&replacement), 1, "written once");
    assert_ne!(managed.bytes(&new), before, "written");
}

/// Which new albums in a held folder a run end leaves unmanaged: one
/// matched to the edition of an album whose files went since the hold
/// began, where the retag join would leave it so (the old album written by
/// the app or taken over: `files_changed`; its edition a person's choice:
/// `retag_followed`). An old album the app never wrote and nobody chose,
/// one gone before the hold, another edition, a new album not matched yet
/// and one with a file the app has a record of are left as they were. A
/// held album matched after its folder is let go is judged at its first
/// look.
#[tokio::test]
async fn a_held_album_is_left_alone_where_the_retag_join_would_leave_it() {
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-held-judged", |_| {}).await;
    let cases = [
        ("written", Some("files_changed")),
        ("taken over", Some("files_changed")),
        ("chosen", Some("retag_followed")),
        ("automatic", None),
        ("gone before the hold", None),
        ("not matched yet", None),
        ("another edition", None),
        ("handed back", None),
    ];
    let mut olds = Vec::new();
    for index in 0..cases.len() {
        olds.push(managed.add_album(&format!("p{index}"), 3).await);
    }
    let db = managed.db();
    managed.match_by_the_app(&olds[3].0);
    managed.activate().await;
    for (album, tracks) in &olds {
        managed.look(album).await;
        assert!(managed.recorded(&tracks[0]).is_some(), "written");
    }

    // Each album retagged and renamed beside itself and removed in the
    // app, then the new album scanned and matched.
    let mut news = Vec::new();
    for (index, (album, tracks)) in olds.iter().enumerate() {
        let paths = managed.retag_into(
            tracks,
            &format!("Retagged {index}"),
            &format!("p{index}/new"),
        );
        Mutations::new(&managed.library)
            .remove_album(album, false, true, "admin")
            .expect("removed in the app");
        rescan(&managed.library).await;
        let new: Vec<String> = paths
            .iter()
            .map(|path| indexed_at(&managed.library, path))
            .collect();
        let replacement = managed.album_of(&new[0]);
        match cases[index].0 {
            "not matched yet" => {
                db.execute(
                    "UPDATE library_identify_jobs SET not_before_ms = ?2 \
                     WHERE local_album_id = ?1 AND state IN ('queued', 'deferred')",
                    rusqlite::params![replacement, i64::MAX],
                )
                .expect("job left queued");
            }
            "another edition" => {
                let mut recall = fitting_release(&managed.library, &replacement);
                recall.releases[0].id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".to_owned();
                recall.releases[0].release_group_id = "rg-elsewhere".to_owned();
                managed.scratch.identify.set_recall(recall);
                while managed.library.identification.identify_tick().await > 0 {}
            }
            _ => managed.identify_as(&replacement, album).await,
        }
        news.push((replacement, new));
    }

    // Each pair under a hold of its own folder, set before the old album
    // was removed, and the old albums changed as each case says.
    let gone_at = |album: &str, edge: &str| -> i64 {
        db.query_row(
            &format!(
                "SELECT CAST({edge}(missing_since) * 1000 AS INTEGER) FROM local_tracks \
                 WHERE local_album_id = ?1"
            ),
            [album],
            |row| row.get(0),
        )
        .expect("missing since")
    };
    for (index, ((album, _), (replacement, new))) in olds.iter().zip(&news).enumerate() {
        let held_at = if cases[index].0 == "gone before the hold" {
            let held_at = gone_at(album, "MAX") + 1;
            let scanned: i64 = db
                .query_row(
                    "SELECT CAST(MIN(imported_at) * 1000 AS INTEGER) FROM local_tracks \
                     WHERE local_album_id = ?1",
                    [replacement],
                    |row| row.get(0),
                )
                .expect("first indexed");
            assert!(held_at <= scanned, "held before the new files came");
            held_at
        } else {
            gone_at(album, "MIN") - 1
        };
        db.execute(
            "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
             VALUES ('music', ?1, ?2)",
            rusqlite::params![format!("p{index}"), held_at],
        )
        .expect("held");
        let clear_records = || {
            db.execute(
                "UPDATE library_track_management_state SET written_sha256 = NULL \
                 WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
                [album],
            )
            .expect("records cleared")
        };
        match cases[index].0 {
            "taken over" => {
                clear_records();
                db.execute(
                    "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
                     VALUES (?1, 'files_changed', 1)",
                    [album],
                )
                .expect("taken over");
            }
            "chosen" | "automatic" => {
                clear_records();
            }
            "handed back" => {
                db.execute(
                    "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                     VALUES (?1, 'recorded at hand-back') ON CONFLICT (local_track_id) \
                     DO UPDATE SET written_sha256 = excluded.written_sha256",
                    [&new[0]],
                )
                .expect("recorded");
            }
            _ => {}
        }
    }

    rescan(&managed.library).await;
    for ((case, mark), (replacement, _)) in cases.iter().zip(&news) {
        assert_eq!(managed.mark(replacement).as_deref(), *mark, "{case}");
    }
    assert_eq!(
        managed
            .held_scopes()
            .into_iter()
            .map(|(_, path, _)| path)
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "every folder is let go, the album waiting for its match too"
    );

    let (waiting, _) = &news[5];
    db.execute(
        "UPDATE library_identify_jobs SET not_before_ms = 0 WHERE local_album_id = ?1",
        [waiting],
    )
    .expect("job due");
    managed.identify_as(waiting, &olds[5].0).await;
    managed.look(waiting).await;
    assert_eq!(managed.mark(waiting).as_deref(), Some("files_changed"));
    assert_eq!(managed.runs_for(waiting), 0);
    assert_eq!(managed.held_scopes(), Vec::new());
}

/// A retag that deleted a file, whose new album is only a best guess while
/// the folder is held: no scan's end can judge it. Removing the old album
/// lets the folder go and writes nothing. When a person confirms the match,
/// the first look leaves the album unmanaged as the person's retag before
/// anything is written; "Manage it again" then writes it.
#[tokio::test]
async fn a_best_guess_confirmed_after_its_folder_is_let_go_is_left_alone() {
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-held-later", |_| {}).await;
    let (album, old) = managed.add_album("first", 12).await;
    managed.match_by_the_app(&album);
    managed.activate().await;
    managed.look(&album).await;
    assert!(
        managed.recorded(&old[0]).is_some(),
        "the app wrote the files"
    );

    let mut paths = managed.retag_into(&old, "Retagged", "new");
    let deleted = paths.pop().expect("twelve files");
    std::fs::remove_file(managed.music.join(deleted)).expect("deleted for good");
    rescan(&managed.library).await;
    assert!(managed.latest_scan_held_back(), "12 gone of 23");
    let new: Vec<String> = paths
        .iter()
        .map(|path| indexed_at(&managed.library, path))
        .collect();
    let replacement = managed.album_of(&new[0]);
    assert_ne!(replacement, album);
    managed.identify_as(&replacement, &album).await;
    let state: String = managed
        .db()
        .query_row(
            "SELECT state FROM library_album_match_state WHERE local_album_id = ?1",
            [&replacement],
            |row| row.get(0),
        )
        .expect("a best guess");
    assert_eq!(state, "unconfirmed");
    let before = managed.bytes(&new);

    rescan(&managed.library).await;
    assert!(managed.latest_scan_held_back(), "still 12 gone of 23");
    assert_eq!(managed.mark(&replacement), None, "nothing to judge on yet");

    Mutations::new(&managed.library)
        .remove_album(&album, false, true, "admin")
        .expect("removed in the app");
    rescan(&managed.library).await;
    assert!(!managed.latest_scan_held_back());
    assert_eq!(managed.held_scopes(), Vec::new(), "the folder is let go");
    assert_eq!(managed.mark(&replacement), None);
    managed.look(&replacement).await;
    assert_eq!(managed.runs_for(&replacement), 0);
    assert_eq!(managed.bytes(&new), before, "nothing written");
    assert!(
        managed
            .skipped_codes()
            .contains(&(replacement.clone(), "UNCONFIRMED_MATCH".to_owned()))
    );

    crate::common::library::operations(&managed.library)
        .confirm_match(&replacement, "admin")
        .expect("match confirmed");
    managed.worker_looks().await;
    assert_eq!(
        managed.mark(&replacement).as_deref(),
        Some("files_changed"),
        "left to the person at its first look"
    );
    assert_eq!(managed.runs_for(&replacement), 0);
    assert_eq!(managed.bytes(&new), before, "still nothing written");

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&replacement, "admin")
            .expect("handed back")
    );
    managed.worker_looks().await;
    assert_eq!(managed.mark(&replacement), None);
    assert_eq!(managed.runs_for(&replacement), 1, "written once");
    assert_ne!(managed.bytes(&new), before, "written");
}

/// Which new albums from a held folder the first look leaves unmanaged when
/// their match settled only after the folder was let go: the same ones a
/// scan's end would, judged on the edition the look is about to write. The
/// old album written by the app gives `files_changed`; one left unmanaged
/// gives `retag_followed` unless the new album's match is a person's; a
/// person's match on both, or another edition, is written. A best guess is
/// judged once unconfirmed matches are managed, and an album in a folder
/// managed by hand is judged too, so a preview of the whole folder leaves
/// it out.
#[tokio::test]
async fn a_held_album_matched_after_its_folder_is_let_go_is_judged_at_its_look() {
    use droppedneedle_kernel::runtime_config::sections::EditionPreferences;
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-held-look", |_| {}).await;
    // (case, the new album's mark, its runs)
    let cases = [
        ("written", Some("files_changed"), 0),
        ("left unmanaged", Some("retag_followed"), 0),
        ("left unmanaged, new chosen", None, 1),
        ("both chosen", None, 1),
        ("another edition", None, 1),
        ("best guess", Some("files_changed"), 0),
        ("folder by hand", Some("files_changed"), 0),
    ];
    let mut olds = Vec::new();
    for index in 0..cases.len() {
        olds.push(managed.add_album(&format!("p{index}"), 3).await);
    }
    let db = managed.db();
    managed.activate().await;
    for (album, tracks) in &olds {
        managed.look(album).await;
        assert!(managed.recorded(&tracks[0]).is_some(), "written");
    }

    // Each album retagged and renamed beside itself and removed in the
    // app, then the new album scanned, its match still to come.
    let mut news = Vec::new();
    for (index, (album, tracks)) in olds.iter().enumerate() {
        let paths = managed.retag_into(
            tracks,
            &format!("Retagged {index}"),
            &format!("p{index}/new"),
        );
        Mutations::new(&managed.library)
            .remove_album(album, false, true, "admin")
            .expect("removed in the app");
        rescan(&managed.library).await;
        let new = indexed_at(&managed.library, &paths[0]);
        let replacement = managed.album_of(&new);
        db.execute(
            "UPDATE library_identify_jobs SET not_before_ms = ?2 \
             WHERE local_album_id = ?1 AND state IN ('queued', 'deferred')",
            rusqlite::params![replacement, i64::MAX],
        )
        .expect("job left queued");
        news.push(replacement);
    }

    // Each pair held from before the old album was removed, the first pair
    // under two holds. One clean scan lets every folder go and has nothing
    // to judge on.
    for (index, (album, _)) in olds.iter().enumerate() {
        let held_at: i64 = db
            .query_row(
                "SELECT CAST(MIN(missing_since) * 1000 AS INTEGER) - 1 FROM local_tracks \
                 WHERE local_album_id = ?1",
                [album],
                |row| row.get(0),
            )
            .expect("missing since");
        let mut folders = vec![format!("p{index}")];
        if index == 0 {
            folders.push("p0/new".to_owned());
        }
        for folder in folders {
            db.execute(
                "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
                 VALUES ('music', ?1, ?2)",
                rusqlite::params![folder, held_at],
            )
            .expect("held");
        }
    }
    rescan(&managed.library).await;
    assert_eq!(managed.held_scopes(), Vec::new(), "every folder let go");
    for replacement in &news {
        assert_eq!(managed.mark(replacement), None, "nothing to judge on");
    }

    let clear_records = |album: &str| {
        db.execute(
            "UPDATE library_track_management_state SET written_sha256 = NULL \
             WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            [album],
        )
        .expect("records cleared");
    };
    let leave_unmanaged = |album: &str| {
        clear_records(album);
        managed.match_by_the_app(album);
        db.execute(
            "INSERT INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) VALUES (?1, 'leave unmanaged', 'admin', 1)",
            [album],
        )
        .expect("left unmanaged");
    };
    let chosen_by_a_person = |album: &str| {
        db.execute(
            "UPDATE local_album_external_identities SET decision_source = 'manual', \
             selected_by_user_id = 'admin' WHERE local_album_id = ?1",
            [album],
        )
        .expect("a person's match");
    };
    for (index, ((album, _), replacement)) in olds.iter().zip(&news).enumerate() {
        let (case, mark, runs) = cases[index];
        db.execute(
            "UPDATE library_identify_jobs SET not_before_ms = 0 WHERE local_album_id = ?1",
            [replacement],
        )
        .expect("job due");
        match case {
            "another edition" => {
                let mut recall = fitting_release(&managed.library, replacement);
                recall.releases[0].id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".to_owned();
                recall.releases[0].release_group_id = "rg-elsewhere".to_owned();
                managed.scratch.identify.set_recall(recall);
                while managed.library.identification.identify_tick().await > 0 {}
            }
            "folder by hand" => {
                managed.set_automatic(false, false);
                managed.identify_as(replacement, album).await;
            }
            _ => managed.identify_as(replacement, album).await,
        }
        match case {
            "left unmanaged" => leave_unmanaged(album),
            "left unmanaged, new chosen" => {
                leave_unmanaged(album);
                chosen_by_a_person(replacement);
            }
            "both chosen" => {
                clear_records(album);
                chosen_by_a_person(replacement);
            }
            "best guess" => {
                db.execute(
                    "INSERT INTO library_album_match_state (local_album_id, state, \
                     reason_code, updated_at) VALUES (?1, 'unconfirmed', 'LOW_CONFIDENCE', 0) \
                     ON CONFLICT (local_album_id) DO UPDATE SET state = excluded.state",
                    [replacement],
                )
                .expect("a best guess");
                managed.look(replacement).await;
                assert_eq!(managed.mark(replacement), None, "{case}");
                assert_eq!(managed.runs_for(replacement), 0, "{case}");
                assert!(
                    managed
                        .skipped_codes()
                        .contains(&(replacement.clone(), "UNCONFIRMED_MATCH".to_owned())),
                    "{case}"
                );
                let mut preferences = managed
                    .library
                    .settings
                    .store()
                    .get::<EditionPreferences>()
                    .expect("preferences read");
                preferences.manage_unconfirmed = true;
                managed
                    .library
                    .settings
                    .store()
                    .save_blocking(preferences)
                    .expect("preferences saved");
            }
            _ => {}
        }
        managed.look(replacement).await;
        assert_eq!(managed.mark(replacement).as_deref(), mark, "{case}");
        assert_eq!(managed.runs_for(replacement), runs, "{case}");
    }

    let by_hand = &news[6];
    let (_, items) = managed
        .preview(SelectionKind::Roots, &["music"], "whole folder")
        .await;
    let left_out: Vec<_> = items
        .iter()
        .filter(|item| &item.album_id == by_hand)
        .collect();
    assert!(!left_out.is_empty(), "in the preview");
    for item in left_out {
        assert_eq!(item.eligibility, Eligibility::Blocked);
        assert_eq!(item.reason().expect("a reason").code, "TAKEN_OVER");
    }
}

/// An album whose edition a person chose, never written by the app,
/// retagged without its ids and renamed: joined at the seal, the person's
/// match moves to the new album in place of the app's, which loses its
/// automatic undo, and automatic management leaves the album alone.
#[tokio::test]
async fn a_persons_edition_retagged_without_its_ids_is_left_alone_after_the_join() {
    let managed = ManagedLibrary::start("lib-join-chosen", |_| {}).await;
    let (album, old) = managed.add_album("first", 2).await;
    managed.activate().await;
    let db = managed.db();
    let chosen = choice_row(&db, &album);
    assert_eq!(chosen.0, "manual");
    assert!(managed.recorded(&old[0]).is_none(), "never written");

    let new = managed.retag_without_ids(&old).await;
    let own_row = managed.album_of(&new[0]);
    let bytes: Vec<String> = new
        .iter()
        .map(|track| file_sha256(&managed.path(track)))
        .collect();
    managed.identify_as(&own_row, &album).await;

    assert_eq!(retired_into(&db, &album), Some(own_row.clone()));
    let moved = choice_row(&db, &own_row);
    assert_eq!(
        (moved.0.as_str(), &moved.1, &moved.2),
        ("manual", &chosen.1, &chosen.2)
    );
    let undo: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_automatic_edition_undo WHERE local_album_id = ?1",
            [&own_row],
            |row| row.get(0),
        )
        .expect("undo read");
    assert_eq!(undo, 0);
    assert!(managed.stamped(&own_row));

    managed.look(&own_row).await;
    assert_eq!(managed.mark(&own_row).as_deref(), Some("retag_followed"));
    let after: Vec<String> = new
        .iter()
        .map(|track| file_sha256(&managed.path(track)))
        .collect();
    assert_eq!(after, bytes, "nothing written");
}

/// Two copies of a track, both mapped to its release track (as an import
/// maps a download beside the old file): nothing moves while both are
/// there. The original deleted, its history is on the copy at that scan's
/// end.
#[tokio::test]
async fn a_deleted_copy_leaves_its_history_with_the_copy_that_stays() {
    let (scratch, library, music, album, old) = album_at_rest("lib-continue-copy", 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    let release_track = placement(&db, &old[1]).1.expect("original mapped");
    std::fs::copy(
        music.join("album/02.flac"),
        music.join("album/02 copy.flac"),
    )
    .expect("copied");
    rescan(&library).await;
    let copy = indexed_at(&library, "album/02 copy.flac");
    assert_eq!(
        library
            .scan_store
            .album_for_track(&copy)
            .unwrap()
            .as_deref(),
        Some(album.as_str())
    );
    db.execute_batch(&format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_track_mbid, decision_source, selected_at) VALUES \
         ('{copy}', 'musicbrainz', 'rec-copy', '{release_track}', 'automatic', 1) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         release_track_mbid = excluded.release_track_mbid; \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        old[1]
    ))
    .expect("both copies mapped");
    rescan(&library).await;
    assert_eq!(holdings(&db, &old[1])[1], 1, "both there: nothing moves");

    std::fs::remove_file(music.join("album/02.flac")).expect("original deleted");
    rescan(&library).await;
    assert_eq!(holdings(&db, &copy)[1], 1);
    assert_eq!(holdings(&db, &old[1])[1], 0);
}

/// A tag reader that holds the scan's first file read until the test lets
/// it go, then reads the file as the app does.
struct HoldFirstRead {
    armed: std::sync::atomic::AtomicBool,
    reached: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl droppedneedle_library::scan::TagReader for HoldFirstRead {
    fn read_tags(
        &self,
        path: &Path,
    ) -> Result<droppedneedle_library::scan::ScannedTags, droppedneedle_library::scan::TagReadError>
    {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            if let Ok(reached) = self.reached.lock() {
                let _ = reached.send(());
            }
            if let Ok(release) = self.release.lock() {
                let _ = release.recv_timeout(std::time::Duration::from_secs(60));
            }
        }
        droppedneedle_library::scan::TagReader::read_tags(
            &droppedneedle_library::adapters::LoftyTagReader,
            path,
        )
    }
}

/// Scan `music` once, held in its index phase: once the walk is over and
/// the first file read waits, `during` runs, then the scan finishes. A
/// new file is planted first so the index phase has a file to read.
async fn scan_held_while(
    library: &LibrarySetup,
    music: &Path,
    during: impl FnOnce() + Send + 'static,
) {
    use droppedneedle_library::scan::{
        BlockingPool, LibraryScanCoordinator, ScanKind, ScanRequest, ScanTrigger,
    };
    plant(music, "zz-new/01.flac", "flac_no_tags.flac");
    let (reached_tx, reached) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let coordinator = Arc::new(LibraryScanCoordinator::new(
        library.scan_store.clone(),
        BlockingPool::new(4),
        Arc::new(HoldFirstRead {
            armed: std::sync::atomic::AtomicBool::new(true),
            reached: std::sync::Mutex::new(reached_tx),
            release: std::sync::Mutex::new(release_rx),
        }),
        library.registry.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    ));
    let registry = library.core.live_registry();
    coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    let paths = HashMap::from([("music".to_owned(), music.to_owned())]);
    let driver = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            while let Some(run) = coordinator.run_once(&paths).await {
                if run.terminal_at.is_some() {
                    break;
                }
            }
            coordinator.run_once(&paths).await;
        })
    };
    tokio::task::spawn_blocking(move || {
        reached
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the scan's first read is reached after its walk");
        during();
        release.send(()).expect("released");
    })
    .await
    .expect("the write runs");
    tokio::time::timeout(std::time::Duration::from_secs(60), driver)
        .await
        .expect("the scan finishes")
        .expect("the scan task");
}

/// `count` untagged files in `filler/`, scanned, so the scope holds
/// plenty of tracks the walk sees and the mass-missing guard never holds
/// it back.
async fn plant_filler(library: &LibrarySetup, music: &Path, count: usize) {
    for number in 1..=count {
        plant(
            music,
            &format!("filler/{number:02}.flac"),
            "flac_no_tags.flac",
        );
    }
    rescan(library).await;
}

/// An album at rest with a second copy of its second track, both mapped
/// to the one release track, and plenty of other tracks in the folder.
/// Returns the library, the album, the original and the copy.
async fn album_with_a_copy(tag: &str) -> (Scratch, LibrarySetup, PathBuf, String, String, String) {
    let (scratch, library, music, album, old) = album_at_rest(tag, 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    let release_track = placement(&db, &old[1]).1.expect("original mapped");
    std::fs::copy(
        music.join("album/02.flac"),
        music.join("album/02 copy.flac"),
    )
    .expect("copied");
    plant_filler(&library, &music, 20).await;
    let copy = indexed_at(&library, "album/02 copy.flac");
    db.execute(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, release_track_mbid, decision_source, selected_at) VALUES \
         (?1, 'musicbrainz', 'rec-copy', ?2, 'automatic', 1) \
         ON CONFLICT (local_track_id, provider) DO UPDATE SET \
         release_track_mbid = excluded.release_track_mbid",
        rusqlite::params![copy, release_track],
    )
    .expect("both copies mapped");
    let original = old[1].clone();
    (scratch, library, music, album, original, copy)
}

/// Give `track` one play, one favourite and one playlist entry.
fn give_history(db: &rusqlite::Connection, album: &str, track: &str) {
    db.execute_batch(&format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES \
         ('play-{track}', 'listener', '{track}', '{album}', 'Two', 'A', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{track}', 1); \
         INSERT INTO playlists (id, name, created_at, updated_at) \
         VALUES ('list-{track}', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, library_file_id, source_type, created_at) \
         VALUES ('entry-{track}', 'list-{track}', 0, 'Two', 'A', 'B', '{album}', '{track}', \
         '{track}', 'local', '');"
    ))
    .expect("history given");
}

fn track_aliases(db: &rusqlite::Connection) -> i64 {
    db.query_row("SELECT COUNT(*) FROM local_track_aliases", [], |row| {
        row.get(0)
    })
    .expect("aliases read")
}

/// A download imported while a scan sits in its index phase, into a folder
/// the walk already passed, is never judged gone by that scan: its tracks
/// stay listed on their own album, and the next scan keeps them.
#[tokio::test]
async fn an_import_during_a_scan_is_not_judged_gone() {
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};

    let (scratch, library, music, album, _) = album_at_rest("lib-fence-import", 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    plant_filler(&library, &music, 20).await;
    let release = library
        .identify_store
        .release(REL_1, None)
        .unwrap()
        .expect("release stored");
    let files: Vec<ImportSource> = [0usize, 1]
        .iter()
        .map(|&track| {
            let source = scratch
                .to_path_buf()
                .join(format!("incoming/task/{track}.flac"));
            std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
            std::fs::copy(fixtures_dir().join("flac_full_02.flac"), &source)
                .expect("download planted");
            ImportSource {
                path: source,
                track,
                replaces: Vec::new(),
            }
        })
        .collect();
    let imported = Arc::new(std::sync::Mutex::new(None));
    {
        let (library, imported) = (library.clone(), imported.clone());
        scan_held_while(&library.clone(), &music, move || {
            let done = library
                .imports
                .import_download(&DownloadImport {
                    task_id: "task".to_owned(),
                    origin: ImportOrigin::Download,
                    staging: None,
                    release,
                    files,
                    extras: Vec::new(),
                    actor: None,
                })
                .expect("imported");
            *imported.lock().expect("lock") = Some(done);
        })
        .await;
    }
    let imported = imported.lock().expect("lock").take().expect("import ran");
    let tracks: Vec<(String, String)> = imported
        .paths
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&music)
                .expect("under the root")
                .to_string_lossy()
                .into_owned();
            db.query_row(
                "SELECT id, availability FROM local_tracks \
                 WHERE root_id = 'music' AND relative_path = ?1",
                [&relative],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("imported track row")
        })
        .collect();
    for (track, availability) in &tracks {
        assert_eq!(availability, "indexed", "imported track {track} is listed");
        assert_eq!(
            library
                .scan_store
                .album_for_track(track)
                .unwrap()
                .as_deref(),
            Some(imported.album_id.as_str()),
            "imported track {track} keeps its album"
        );
    }

    rescan(&library).await;
    for (track, _) in &tracks {
        assert_eq!(
            availability_of(&db, track),
            "indexed",
            "{track} after the next scan"
        );
        assert_eq!(
            library
                .scan_store
                .album_for_track(track)
                .unwrap()
                .as_deref(),
            Some(imported.album_id.as_str())
        );
    }
}

/// A person's rename of one of two copies of a release track, applied
/// while a scan sits in its index phase, keeps the track: it is listed at
/// its new path, keeps its plays, favourite and playlist entry, and no
/// alias hands it to the other copy.
#[tokio::test]
async fn a_rename_during_a_scan_keeps_the_tracks_history() {
    use droppedneedle_library::publish::planner::PlanKind;

    let (scratch, library, music, album, original, copy) =
        album_with_a_copy("lib-fence-rename").await;
    let db = scratch_db(&scratch);
    give_history(&db, &album, &original);
    assert_eq!(holdings(&db, &original)[..3], [1, 1, 1]);
    {
        let (library, album) = (library.clone(), album.clone());
        scan_held_while(&library.clone(), &music, move || {
            publish_one(
                &library,
                PlanKind::Move,
                &album,
                "album/02.flac",
                Some("moved/02.flac"),
                &[],
            );
        })
        .await;
    }

    let row = old_row(&db, &original);
    assert_eq!(
        (row.1.as_str(), row.2.as_str()),
        ("moved/02.flac", "indexed"),
        "the renamed track is listed at its new path"
    );
    assert_eq!(
        holdings(&db, &original)[..3],
        [1, 1, 1],
        "its history stays"
    );
    assert_eq!(holdings(&db, &copy)[..3], [0, 0, 0]);
    assert_eq!(track_aliases(&db), 0, "nothing hands it to the copy");
}

/// A removal of one copy of a release track, stopped after its file was
/// set aside and before its commit, stays open while a scan walks past
/// the empty path. Put back by recovery before the scan's end, the copy
/// is not marked missing and keeps its favourite.
#[tokio::test]
async fn a_removal_open_across_a_walk_is_not_judged_gone() {
    use droppedneedle_library::mutations::Mutations;
    use droppedneedle_library::publish::CrashPoint;

    let (scratch, library, music, album, original, copy) =
        album_with_a_copy("lib-fence-removal").await;
    add_admin(&library);
    let db = scratch_db(&scratch);
    give_history(&db, &album, &copy);
    let home = music.join("album/02 copy.flac");

    library
        .publishing
        .arm_publish_crash(Some(CrashPoint::BeforeCatalogCommit));
    let answer = Mutations::new(&library).remove_track_now(&copy, true, "admin");
    library.publishing.arm_publish_crash(None);
    assert!(answer.is_err(), "the removal stopped before its commit");
    assert!(!home.exists(), "its file is set aside");
    {
        let library = library.clone();
        let copy = copy.clone();
        scan_held_while(&library.clone(), &music, move || {
            // The copy's row changed while the removal waited, so recovery
            // puts its file back.
            library
                .scan_store
                .db()
                .run_sql(&format!(
                    "UPDATE local_tracks SET stat_revision = 'changed' WHERE id = '{copy}'"
                ))
                .expect("row changes");
            library.publishing.publish_tick().expect("recovery runs");
        })
        .await;
    }

    assert!(home.exists(), "recovery put the file back");
    assert_eq!(availability_of(&db, &copy), "indexed", "the copy is listed");
    assert_eq!(holdings(&db, &copy)[..3], [1, 1, 1], "its history stays");
    assert_eq!(holdings(&db, &original)[..3], [0, 0, 0]);
    assert_eq!(track_aliases(&db), 0);
}

/// A download that brings back a track whose file was deleted maps the
/// new file in the import itself, and the old track's history is on it
/// without a scan.
#[tokio::test]
async fn an_imported_track_takes_the_history_of_the_one_it_replaces() {
    use droppedneedle_library::import::{DownloadImport, ImportOrigin, ImportSource};

    let (scratch, library, _, album, _) = album_at_rest("lib-continue-import", 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    let release = library
        .identify_store
        .release(REL_1, None)
        .unwrap()
        .expect("release stored");
    let import = |task: &str, tracks: &[usize], fixture: &str| {
        let extension = Path::new(fixture).extension().expect("extension");
        let files = tracks
            .iter()
            .map(|&track| {
                let source = scratch
                    .to_path_buf()
                    .join(format!("incoming/{task}/{track}"))
                    .with_extension(extension);
                std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
                std::fs::copy(fixtures_dir().join(fixture), &source).expect("download planted");
                ImportSource {
                    path: source,
                    track,
                    replaces: Vec::new(),
                }
            })
            .collect();
        library
            .imports
            .import_download(&DownloadImport {
                task_id: task.to_owned(),
                origin: ImportOrigin::Download,
                staging: None,
                release: release.clone(),
                files,
                extras: Vec::new(),
                actor: None,
            })
            .expect("imported")
    };
    let first = import("task-1", &[0, 1, 2], "flac_full_02.flac");
    let tracks: Vec<String> = first
        .paths
        .iter()
        .map(|path| {
            let relative = path
                .strip_prefix(scratch.to_path_buf().join("music"))
                .expect("under the root")
                .to_string_lossy()
                .into_owned();
            indexed_at(&library, &relative)
        })
        .collect();
    std::fs::remove_file(&first.paths[1]).expect("file deleted");
    rescan(&library).await;
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        tracks[1]
    ))
    .expect("starred");

    // In another format, so the new file has a path of its own.
    let again = import("task-2", &[1], "mp3_full_01.mp3");
    assert_eq!(
        again.album_id, first.album_id,
        "the download joins the album"
    );
    let relative = again.paths[0]
        .strip_prefix(scratch.to_path_buf().join("music"))
        .expect("under the root")
        .to_string_lossy()
        .into_owned();
    let new = indexed_at(&library, &relative);
    assert_ne!(new, tracks[1]);
    assert_eq!(holdings(&db, &new)[1], 1);
    assert_eq!(holdings(&db, &tracks[1])[1], 0);
}

/// A review approval of the release a copy of a gone track belongs to
/// moves the original's history to the copy once the files are placed on
/// that release, with no scan. The copy lands on the original's release
/// track, with the release.
#[tokio::test]
async fn an_approval_continues_a_track_once_its_files_are_placed() {
    use droppedneedle_library::identify::models::{
        CandidateEvidence, EvidenceClass, TrackEvidence,
    };

    let (scratch, library, music, album, old) = album_at_rest("lib-continue-approve", 3).await;
    let db = scratch_db(&scratch);
    scratch
        .identify
        .set_recall(fitting_release(&library, &album));
    while library.identification.identify_tick().await > 0 {}
    let release_track = placement(&db, &old[1]).1.expect("original mapped");
    std::fs::copy(
        music.join("album/02.flac"),
        music.join("album/02 copy.flac"),
    )
    .expect("copied");
    rescan(&library).await;
    std::fs::remove_file(music.join("album/02.flac")).expect("original deleted");
    rescan(&library).await;
    let copy = indexed_at(&library, "album/02 copy.flac");
    assert_eq!(album_row(&db, &copy), album);
    assert_eq!(placement(&db, &copy).1, None, "the copy is not mapped yet");
    db.execute_batch(&format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        old[1]
    ))
    .expect("starred");

    let key = format!("rg-1:{REL_1}");
    droppedneedle_library::identify::review::file_review(
        &library.identify_store,
        "rev-approve",
        &album,
        "EDITION_CLEARED_CONFLICT",
        vec![CandidateEvidence {
            candidate_key: key.clone(),
            release_group_mbid: "rg-1".to_owned(),
            release_mbid: Some(REL_1.to_owned()),
            album_title: "Album".to_owned(),
            album_artist_name: String::new(),
            track_evidence: vec![TrackEvidence {
                local_track_id: copy.clone(),
                classification: EvidenceClass::Supported,
                evidence_kinds: Vec::new(),
                recording_mbid: Some("rec-copy".to_owned()),
                release_track_mbid: Some(release_track.clone()),
            }],
            score: 1.0,
            reason_code: "EDITION_CLEARED_CONFLICT".to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        }],
    )
    .unwrap();
    library
        .identification
        .approve_review("rev-approve", "curator", &key)
        .expect("approved");
    remap_once(
        &library,
        &scratch.identify,
        fitting_release(&library, &album),
    )
    .await;
    let (release, mapped, _) = placement(&db, &copy);
    assert_eq!(
        (release.as_deref(), mapped),
        (Some(REL_1), Some(release_track)),
        "placed on the approved release"
    );
    assert_eq!(holdings(&db, &copy)[1], 1);
    assert_eq!(holdings(&db, &old[1])[1], 0);
}

// ---------------------------------------------------------------------------
// Album rows a scan would not join: a person joins or dismisses them.
// ---------------------------------------------------------------------------

/// Both albums of a pair and the pair row, to show nothing changed.
fn pair_snapshot(db: &rusqlite::Connection, old: &str, new: &str) -> String {
    use rusqlite::OptionalExtension as _;
    let mut out = String::new();
    for album in [old, new] {
        let row: (i64, Option<String>) = db
            .query_row(
                "SELECT row_revision, retired_into_album_id FROM local_albums WHERE id = ?1",
                [album],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("album row");
        let identity: Option<ChoiceRow> = db
            .query_row(
                "SELECT decision_source, release_mbid, selected_by_user_id, selected_at, \
                 row_revision FROM local_album_external_identities WHERE local_album_id = ?1",
                [album],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .expect("identity reads");
        let custom: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM library_custom_edition_active WHERE local_album_id = ?1",
                [album],
                |row| row.get(0),
            )
            .expect("custom reads");
        out.push_str(&format!("{row:?} {identity:?} {custom};"));
    }
    let pair: Option<(String, String, i64)> = db
        .query_row(
            "SELECT reason, state, updated_at FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND new_album_id = ?2",
            [old, new],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .expect("pair reads");
    format!("{out} {pair:?}")
}

/// The album's pending reviews, as (reason, candidates).
fn pending_reviews(db: &rusqlite::Connection, album: &str) -> Vec<(String, String)> {
    db.prepare(
        "SELECT reason_code, candidates_json FROM library_identify_reviews \
         WHERE local_album_id = ?1 AND state = 'pending'",
    )
    .expect("reviews query")
    .query_map([album], |row| Ok((row.get(0)?, row.get(1)?)))
    .expect("reviews read")
    .collect::<Result<_, _>>()
    .expect("reviews rows")
}

/// The pairs a scan left apart for a person: the read lists the ones a
/// person decides on, a join drops a custom edition and the chosen
/// edition not kept (each with a review on the album that stays) and
/// merges the two, a refusal changes nothing, and a dismissed pair is no
/// longer listed. Only an admin reaches them.
#[tokio::test]
async fn retag_pairs_list_join_and_dismiss() {
    let rig = Rig::start("lib-retag-pairs").await;
    let app = &rig.app;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    let music = app.config.root_app_dir.join("music");
    let folders = [
        "a-old", "a-new", "b-old", "b-new", "c-old", "c-new", "d-old", "d-new", "e-old", "e-new",
        "f-old", "f-new", "f-third", "g-one", "g-two", "h-old", "h-new", "i-old", "i-new",
    ];
    for folder in folders {
        plant(&music, &format!("{folder}/01.flac"), "flac_full_01.flac");
    }
    rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    app.scan_library().await;
    let track = |folder: &str| {
        rig.scan_store
            .track_at("music", &format!("{folder}/01.flac"))
            .unwrap()
            .expect("track indexed")
    };
    let ids: std::collections::BTreeMap<&str, String> = folders
        .iter()
        .map(|folder| {
            let album = rig
                .scan_store
                .album_for_track(&track(folder))
                .unwrap()
                .expect("album");
            (*folder, album)
        })
        .collect();
    let id = |folder: &str| ids[folder].clone();
    assert_eq!(
        ids.values()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        folders.len(),
        "each folder is an album"
    );
    let choose = |folder: &str, group: &str, release: &str| {
        format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision) VALUES ('{}', 'musicbrainz', '{group}', {release}, \
             'manual', 'curator', 1234.5, 3);",
            id(folder)
        )
    };
    let pair = |old: &str, new: &str, reason: &str, at: i64| {
        format!(
            "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES ('{}', '{}', '{reason}', 'open', {at}, {at});",
            id(old),
            id(new)
        )
    };
    let db = app.db();
    db.execute_batch(
        &[
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z');"
                .to_owned(),
            choose("a-old", "rg-a", "'rel-a-old'"),
            choose("a-new", "rg-a", "'rel-a-new'"),
            choose("b-old", "rg-b", "'rel-b-old'"),
            choose("b-new", "rg-b", "'rel-b-new'"),
            choose("d-old", "rg-d", "NULL"),
            choose("d-new", "rg-d", "'rel-d-new'"),
            choose("e-old", "rg-e", "'rel-e-old'"),
            choose("e-new", "rg-e", "'rel-e-new'"),
            format!(
                "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
                 release_group_mbid, album_title, album_artist_name, source_album_revision, \
                 input_revision, content_hash, sealed_by_user_id, sealed_at) \
                 VALUES ('custom-d', '{d}', 1, 'rg-d', 'T', 'A', 1, 'i', 'h', 'curator', 0); \
                 INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
                 activated_at) VALUES ('{d}', 'custom-d', 0);",
                d = id("d-old")
            ),
            format!(
                "INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_mbid, release_track_mbid, decision_source, \
                 selected_at) VALUES ('{}', 'musicbrainz', 'rec-a', 'rel-a-new', 'rt-a', \
                 'manual', 0);",
                track("a-new")
            ),
            format!(
                "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
                 VALUES ('curator', 'album', '{a}', 1); \
                 INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
                 artist_name, played_at) VALUES ('play-a', 'curator', '{a}', 'Song', 'Artist', \
                 '2026-01-01T00:00:00Z'); \
                 INSERT INTO playlists (id, name, created_at, updated_at) \
                 VALUES ('list', 'List', '', ''); \
                 INSERT INTO playlist_tracks (id, playlist_id, position, track_name, \
                 artist_name, album_name, album_id, source_type, created_at) \
                 VALUES ('entry', 'list', 0, 'Song', 'Artist', 'Old', '{a}', 'local', ''); \
                 INSERT INTO library_management_exclusions (local_album_id, reason, \
                 excluded_by_user_id, excluded_at) VALUES ('{a}', 'by hand', 'curator', 5); \
                 INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
                 VALUES ('{a}', 'files_changed', 1234);",
                a = id("a-old")
            ),
            format!(
                "INSERT INTO library_contribution_drafts (id, local_album_id, \
                 created_by_user_id, state, album_row_revision, input_revision, \
                 local_snapshot_json, resolved_draft_json, source_selection_json, created_at, \
                 updated_at) VALUES ('draft-c', '{}', 'curator', 'ready', 1, 'r1', '{{}}', \
                 '{{}}', '{{}}', 1, 1);",
                id("c-old")
            ),
            pair("a-old", "a-new", "chosen_matches_differ", 1000),
            pair("b-old", "b-new", "chosen_matches_differ", 2000),
            pair("c-old", "c-new", "tracks_elsewhere", 3000),
            pair("d-old", "d-new", "custom_edition", 4000),
            pair("e-old", "e-new", "chosen_matches_differ", 5000),
            pair("f-old", "f-new", "tracks_elsewhere", 6000),
            pair("g-one", "g-two", "live_work", 7000),
            pair("g-two", "g-one", "mostly_missing", 8000),
        ]
        .join("\n"),
    )
    .expect("pairs seeded");

    // Only an admin reaches the routes.
    let (a_old, a_new) = (id("a-old"), id("a-new"));
    let join_a = format!("/api/v3/library/retag-pairs/{a_old}/{a_new}/join");
    let routes = [
        ("GET", "/api/v3/library/retag-pairs".to_owned(), None),
        (
            "POST",
            join_a.clone(),
            Some(json!({"expected_identity_revisions": {}})),
        ),
        (
            "POST",
            format!("/api/v3/library/retag-pairs/{a_old}/{a_new}/dismiss"),
            None,
        ),
    ];
    let user = app.login_user("plain").await;
    let trusted = app.login_user("trusty").await;
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/admin/users/{}/role", trusted.user_id),
            &headers,
            Some(json!({"role": "trusted"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (method, path, body) in &routes {
        let (status, _) = app.call(method, path, &[], body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        for caller in [&user, &trusted] {
            let (status, _) = app
                .call(
                    method,
                    path,
                    &[("authorization", caller.authorization.as_str())],
                    body.clone(),
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }

    // The read: open pairs a person decides on, newest first.
    let list = || async {
        let (status, body) = app
            .call("GET", "/api/v3/library/retag-pairs", &headers, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };
    let listed = list().await;
    assert_eq!(listed["total"], json!(6), "{listed}");
    let order: Vec<(String, String, String, bool)> = listed["pairs"]
        .as_array()
        .expect("pairs")
        .iter()
        .map(|pair| {
            (
                pair["old_album"]["id"].as_str().expect("old").to_owned(),
                pair["new_album"]["id"].as_str().expect("new").to_owned(),
                pair["reason"].as_str().expect("reason").to_owned(),
                pair["editions_differ"].as_bool().expect("differ"),
            )
        })
        .collect();
    let expect = |old: &str, new: &str, reason: &str, differ: bool| {
        (id(old), id(new), reason.to_owned(), differ)
    };
    assert_eq!(
        order,
        vec![
            expect("f-old", "f-new", "tracks_elsewhere", false),
            expect("e-old", "e-new", "chosen_matches_differ", true),
            expect("d-old", "d-new", "custom_edition", false),
            expect("c-old", "c-new", "tracks_elsewhere", false),
            expect("b-old", "b-new", "chosen_matches_differ", true),
            expect("a-old", "a-new", "chosen_matches_differ", true),
        ]
    );
    let shown_a = &listed["pairs"][5];
    assert_eq!(
        shown_a["old_album"]["edition"],
        json!({"release_group_mbid": "rg-a", "release_mbid": "rel-a-old", "chosen": true,
               "custom": false})
    );
    assert_eq!(shown_a["old_album"]["identity_revision"], json!(3));
    assert_eq!(shown_a["old_album"]["track_count"], json!(1));
    assert_eq!(
        listed["pairs"][2]["old_album"]["edition"]["custom"],
        json!(true)
    );

    // Refusals change nothing.
    let before = pair_snapshot(&db, &a_old, &a_new);
    let refusals = [
        (
            json!({"keep_edition_of": a_new, "expected_identity_revisions": {&a_old: 3}}),
            StatusCode::BAD_REQUEST,
            "RETAG_PAIR_REVISIONS_REQUIRED",
        ),
        (
            json!({"expected_identity_revisions": {&a_old: 3, &a_new: 3}}),
            StatusCode::BAD_REQUEST,
            "KEEP_EDITION_REQUIRED",
        ),
        (
            json!({"keep_edition_of": a_old,
                   "expected_identity_revisions": {&a_old: 2, &a_new: 3}}),
            StatusCode::CONFLICT,
            "STALE_REVISION",
        ),
    ];
    for (body, status, code) in refusals {
        let (got, answer) = app.call("POST", &join_a, &headers, Some(body)).await;
        assert_eq!(got, status, "{answer}");
        assert_eq!(answer["error"]["code"], json!(code));
        assert_eq!(pair_snapshot(&db, &a_old, &a_new), before, "{code}");
    }

    // Keep the old album's edition: it lands on the new album, which
    // takes the old album's id, people's marks and history.
    let keep_old = json!({"keep_edition_of": a_old,
                          "expected_identity_revisions": {&a_old: 3, &a_new: 3}});
    for _ in 0..2 {
        let (status, body) = app
            .call("POST", &join_a, &headers, Some(keep_old.clone()))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"album_id": a_new}), "a retry answers the same");
    }
    let alias: String = db
        .query_row(
            "SELECT local_album_id FROM local_album_aliases WHERE alias = ?1",
            [&a_old],
            |row| row.get(0),
        )
        .expect("the old id is an alias");
    assert_eq!(alias, a_new, "the old id names the album");
    let chosen = choice_row(&db, &a_new);
    assert_eq!(
        (
            chosen.0.as_str(),
            chosen.1.as_deref(),
            chosen.2.as_deref(),
            chosen.3
        ),
        ("manual", Some("rel-a-old"), Some("curator"), 1234.5)
    );
    let reviews = pending_reviews(&db, &a_new);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert_eq!(reviews[0].0, "EDITION_KEPT_OVER_CONFLICT");
    assert!(reviews[0].1.contains("rel-a-new"), "{}", reviews[0].1);
    assert_eq!(
        remap_row(&db, &a_new).map(|row| row.0).as_deref(),
        Some("place_only")
    );
    let count = |sql: &str| -> i64 {
        db.query_row(sql, [&a_new], |row| row.get(0))
            .expect("count")
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM library_retag_pairs WHERE new_album_id = ?1"),
        0
    );
    for sql in [
        "SELECT COUNT(*) FROM library_user_favorites WHERE item_id = ?1",
        "SELECT COUNT(*) FROM library_play_history WHERE local_album_id = ?1",
        "SELECT COUNT(*) FROM playlist_tracks WHERE album_id = ?1",
        "SELECT COUNT(*) FROM library_management_exclusions WHERE local_album_id = ?1",
        "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = ?1",
    ] {
        assert_eq!(count(sql), 1, "{sql}");
    }

    // The album's history tells the join, and who asked as each caller
    // may know it; the old id is no album of its own any more.
    let old_title: String = db
        .query_row(
            "SELECT title FROM local_albums WHERE id = ?1",
            [&a_old],
            |row| row.get(0),
        )
        .expect("old title");
    let history_a = format!("/api/v3/library/albums/{a_new}/history");
    for (auth, by) in [
        (admin.as_str(), "you"),
        (user.authorization.as_str(), "person"),
    ] {
        let (status, body) = app
            .call("GET", &history_a, &[("authorization", auth)], None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["items"],
            json!([{"kind": "retag_joined", "at": body["items"][0]["at"], "by": by,
                    "track_count": 0, "track_titles": [], "other_album_title": old_title,
                    "undo_id": null}]),
            "{body}"
        );
    }
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/albums/{a_old}/history"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("ALBUM_NOT_FOUND"));

    // Keep the new album's: the mirror case.
    let (b_old, b_new) = (id("b-old"), id("b-new"));
    let b_chosen = choice_row(&db, &b_new);
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{b_old}/{b_new}/join"),
            &headers,
            Some(json!({"keep_edition_of": b_new,
                        "expected_identity_revisions": {&b_old: 3, &b_new: 3}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], json!(b_new));
    assert_eq!(
        choice_row(&db, &b_new),
        b_chosen,
        "the kept edition is as it was"
    );
    let reviews = pending_reviews(&db, &b_new);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert_eq!(reviews[0].0, "EDITION_KEPT_OVER_CONFLICT");
    assert!(reviews[0].1.contains("rel-b-old"), "{}", reviews[0].1);

    // Open work on the old album holds the join back, changing nothing;
    // once it is gone the split joins without an edition to choose.
    let (c_old, c_new) = (id("c-old"), id("c-new"));
    let join_c = format!("/api/v3/library/retag-pairs/{c_old}/{c_new}/join");
    let plain = json!({"expected_identity_revisions": {&c_old: 0, &c_new: 0}});
    let before = pair_snapshot(&db, &c_old, &c_new);
    let (status, body) = app
        .call("POST", &join_c, &headers, Some(plain.clone()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        json!(
            "One of these albums has an open preview or a running operation. Finish or \
             discard it, then merge."
        )
    );
    assert_eq!(pair_snapshot(&db, &c_old, &c_new), before);
    db.execute(
        "UPDATE library_contribution_drafts SET state = 'cancelled' WHERE id = 'draft-c'",
        [],
    )
    .expect("draft closed");
    let (status, body) = app.call("POST", &join_c, &headers, Some(plain)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], json!(c_new));

    // A custom edition on the old album does not carry over; the new
    // album's chosen edition stays and the manifest is kept.
    let (d_old, d_new) = (id("d-old"), id("d-new"));
    let d_chosen = choice_row(&db, &d_new);
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{d_old}/{d_new}/join"),
            &headers,
            Some(json!({"expected_identity_revisions": {&d_old: 3, &d_new: 3}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(choice_row(&db, &d_new), d_chosen);
    let reviews = pending_reviews(&db, &d_new);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert_eq!(reviews[0].0, "EDITION_CLEARED_CUSTOM");
    let total = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).expect("count") };
    assert_eq!(
        total("SELECT COUNT(*) FROM library_custom_edition_active"),
        0
    );
    assert_eq!(
        total("SELECT COUNT(*) FROM library_custom_edition_manifests WHERE id = 'custom-d'"),
        1
    );

    // Dismiss: once, then already; the pair is no longer listed or joined.
    let (e_old, e_new) = (id("e-old"), id("e-new"));
    let dismiss_e = format!("/api/v3/library/retag-pairs/{e_old}/{e_new}/dismiss");
    for dismissed in [true, false] {
        let (status, body) = app.call("POST", &dismiss_e, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"dismissed": dismissed}));
    }
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{e_old}/{e_new}/join"),
            &headers,
            Some(json!({"keep_edition_of": e_old,
                        "expected_identity_revisions": {&e_old: 3, &e_new: 3}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("RETAG_PAIR_DISMISSED"));

    // The new album merged into a third by the album tools: the pair
    // stands for the third album, and the join merges into it.
    let (f_old, f_third) = (id("f-old"), id("f-third"));
    let selection = json!({"track_ids": [track("f-new")], "target_album_id": f_third});
    let (status, preview) = app
        .call(
            "POST",
            "/api/v3/library/albums/merge-preview",
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/albums/merge",
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = list().await;
    assert_eq!(listed["total"], json!(1), "{listed}");
    assert_eq!(listed["pairs"][0]["old_album"]["id"], json!(f_old));
    assert_eq!(listed["pairs"][0]["new_album"]["id"], json!(f_third));
    let f_new = id("f-new");
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{f_old}/{f_new}/join"),
            &headers,
            Some(json!({"expected_identity_revisions": {&f_old: 0, &f_new: 0}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a card shown before: {body}");
    assert_eq!(body["error"]["code"], json!("RETAG_PAIR_ALBUM_GONE"));
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{f_old}/{f_third}/join"),
            &headers,
            Some(json!({"expected_identity_revisions": {&f_old: 0, &f_third: 0}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], json!(f_third));
    let retired: Option<String> = db
        .query_row(
            "SELECT retired_into_album_id FROM local_albums WHERE id = ?1",
            [&f_old],
            |row| row.get(0),
        )
        .expect("album row");
    assert_eq!(retired, Some(f_third));

    // A custom edition on the new album, dropped for the old album's
    // chosen release, leaves the custom edition's review. A refusal after
    // an edition was dropped puts the edition back.
    let (h_old, h_new, i_old, i_new) = (id("h-old"), id("h-new"), id("i-old"), id("i-new"));
    db.execute_batch(
        &[
            choose("h-old", "rg-h", "'rel-h-old'"),
            choose("h-new", "rg-h", "NULL"),
            format!(
                "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
                 release_group_mbid, album_title, album_artist_name, source_album_revision, \
                 input_revision, content_hash, sealed_by_user_id, sealed_at) \
                 VALUES ('custom-h', '{h_new}', 1, 'rg-h', 'T', 'A', 1, 'i', 'h', 'curator', \
                 0); INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
                 activated_at) VALUES ('{h_new}', 'custom-h', 0);"
            ),
            choose("i-old", "rg-i", "'rel-i-old'"),
            choose("i-new", "rg-i", "'rel-i-new'"),
            format!(
                "INSERT INTO library_contribution_drafts (id, local_album_id, \
                 created_by_user_id, state, album_row_revision, input_revision, \
                 local_snapshot_json, resolved_draft_json, source_selection_json, created_at, \
                 updated_at) VALUES ('draft-i', '{i_old}', 'curator', 'ready', 1, 'r1', \
                 '{{}}', '{{}}', '{{}}', 1, 1);"
            ),
            pair("h-old", "h-new", "chosen_matches_differ", 9000),
            pair("i-old", "i-new", "chosen_matches_differ", 9100),
        ]
        .join("\n"),
    )
    .expect("more pairs seeded");
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{h_old}/{h_new}/join"),
            &headers,
            Some(json!({"keep_edition_of": h_old,
                        "expected_identity_revisions": {&h_old: 3, &h_new: 3}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reviews = pending_reviews(&db, &h_new);
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert_eq!(reviews[0].0, "EDITION_CLEARED_CUSTOM");
    let join_i = format!("/api/v3/library/retag-pairs/{i_old}/{i_new}/join");
    let before = pair_snapshot(&db, &i_old, &i_new);
    let (status, body) = app
        .call(
            "POST",
            &join_i,
            &headers,
            Some(json!({"keep_edition_of": i_old,
                        "expected_identity_revisions": {&i_old: 3, &i_new: 3}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("RETAG_PAIR_LIVE_WORK"));
    assert_eq!(
        pair_snapshot(&db, &i_old, &i_new),
        before,
        "the dropped edition is back"
    );
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{i_old}/{i_new}/dismiss"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // No such pair.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/retag-pairs/{}/{b_new}/join", id("g-one")),
            &headers,
            Some(json!({"expected_identity_revisions": {}})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let listed = list().await;
    assert_eq!(listed, json!({"pairs": [], "total": 0}));
    rig.stop().await;
}

/// One recording's credits from a recorded MusicBrainz answer, by its
/// release-track id.
fn probe_credits(
    fixture: &str,
    track_id: &str,
) -> droppedneedle_library::matching::model::RecordingRelations {
    retyped_credits(fixture, track_id, &[])
}

/// [`probe_credits`] with the relationships listed as `(artist, recorded
/// type, new type)` retyped in the JSON first: a type the recordings
/// lack, made from a recorded shape.
fn retyped_credits(
    fixture: &str,
    track_id: &str,
    retype: &[(&str, &str, &str)],
) -> droppedneedle_library::matching::model::RecordingRelations {
    fn retype_in(value: &mut Value, retype: &[(&str, &str, &str)]) {
        match value {
            Value::Object(map) => {
                let artist = map
                    .get("artist")
                    .and_then(|artist| artist.get("name"))
                    .and_then(Value::as_str);
                let kind = map.get("type").and_then(Value::as_str);
                if let Some((_, _, to)) = retype
                    .iter()
                    .find(|(name, from, _)| Some(*name) == artist && Some(*from) == kind)
                {
                    map.insert("type".to_owned(), Value::from(*to));
                }
                map.values_mut().for_each(|child| retype_in(child, retype));
            }
            Value::Array(items) => items.iter_mut().for_each(|item| retype_in(item, retype)),
            _ => {}
        }
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/musicbrainz")
        .join(fixture);
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(path).expect("probe reads")).expect("probe parses");
    retype_in(&mut value, retype);
    let wire: droppedneedle_providers::musicbrainz::MbRelease =
        serde_json::from_value(value).expect("probe decodes");
    droppedneedle_library::identify::sources::relations_from_wire(&wire, 0)
        .tracks
        .remove(track_id)
        .expect("the probe has the track")
}

/// Gould's piano on Beethoven's concerto (composer, conductor, performer,
/// work).
fn concerto_credits() -> droppedneedle_library::matching::model::RecordingRelations {
    probe_credits(
        "release_relations_classical.json",
        "ed4f20f0-61cf-439d-9c64-1d9acb043cbb",
    )
}

/// A remix of "Alejandro" (producer, remixer, composer, lyricist, work).
fn remix_credits() -> droppedneedle_library::matching::model::RecordingRelations {
    probe_credits(
        "release_relations_pop.json",
        "aa235c36-5662-3709-ab97-06a1b4757d7e",
    )
}

/// A remix of "Telephone" (remixer, the work's writers), its two vocals
/// retyped to an engineer and a mix credit.
fn engineered_credits() -> droppedneedle_library::matching::model::RecordingRelations {
    retyped_credits(
        "release_relations_pop.json",
        "bb546d4a-8eb4-3fec-b554-667a448ff053",
        &[
            ("Beyoncé", "vocal", "engineer"),
            ("Lady Gaga", "vocal", "mix"),
        ],
    )
}

impl ManagedLibrary {
    /// Credits MusicBrainz answers for the album's edition: `pick` gives
    /// the probe credits for the n-th file, moved onto that file's
    /// release track and recording.
    fn scripted_credits(
        &self,
        tracks: &[String],
        pick: impl Fn(usize) -> droppedneedle_library::matching::model::RecordingRelations,
    ) -> droppedneedle_library::matching::model::ReleaseRelations {
        let store = &self.library.identify_store;
        let mut relations = droppedneedle_library::matching::model::ReleaseRelations {
            version: droppedneedle_library::matching::model::RELATIONS_VERSION,
            ..Default::default()
        };
        for (index, track) in tracks.iter().enumerate() {
            let identity = store.track_identity(track).unwrap().expect("track mapped");
            let mut credits = pick(index);
            credits.recording_id = identity.recording_mbid.expect("recording");
            relations
                .tracks
                .insert(identity.release_track_mbid.expect("release track"), credits);
        }
        relations
    }
}

/// The Picard profile writes MusicBrainz credits in every format's home
/// from a recorded answer: composer, performer with the instrument (and
/// the performing orchestra), producer, work and work id, and the other
/// roles: engineer, mixer and writer. M4A says it cannot hold a
/// performer. The credits are fetched once and kept on the stored
/// edition: a second plan asks MusicBrainz nothing and changes nothing.
#[tokio::test]
async fn the_picard_profile_writes_musicbrainz_credits() {
    use crate::library_tags::{id3_people, id3_txxx_values, mp4_freeform, vorbis_key_values};
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::tags::{TagField, read_fields};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-credits", |_| {}, scripted.clone()).await;
    let v23 = clean_v23_mp3(&managed.scratch.to_path_buf().join("v23.mp3"));
    let formats = [
        "management_full.flac",
        "management_full.mp3",
        v23.as_str(),
        "management_full.m4a",
    ];
    let fixtures: Vec<&str> = formats
        .iter()
        .chain(formats.iter())
        .chain(formats.iter())
        .copied()
        .collect();
    let (album, tracks) = managed.add_album_from("credits", &fixtures, |_| {}).await;
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |n| match n {
            0..4 => concerto_credits(),
            4..8 => remix_credits(),
            _ => engineered_credits(),
        }));

    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&album], "credits")
        .await;
    assert_eq!(scripted.lookups.lock().expect("calls").len(), 1);
    for item in &items {
        assert!(item.eligibility.writes(), "{:?}", item.facts);
        let homeless = item
            .facts
            .warnings
            .iter()
            .any(|warning| warning.starts_with("\"performer\" cannot hold"));
        assert!(
            !homeless || item.facts.audio_format == "m4a",
            "{:?}",
            item.facts.warnings
        );
    }
    let homeless = items
        .iter()
        .filter(|item| {
            item.facts
                .warnings
                .iter()
                .any(|warning| warning.starts_with("\"performer\" cannot hold"))
        })
        .count();
    assert_eq!(
        homeless, 1,
        "only the concerto's M4A has a performer to refuse"
    );
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");

    let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    for (index, track) in tracks.iter().enumerate() {
        let file = managed.path(track);
        let name = file.display().to_string();
        let fields = read_fields(&file).expect("tags read");
        let get = |field: TagField| fields.get(&field).cloned().unwrap_or_default();
        let m4a = name.ends_with(".m4a");
        if index < 4 {
            assert_eq!(
                get(TagField::Composer),
                owned(&["Ludwig van Beethoven"]),
                "{name}"
            );
            assert_eq!(
                get(TagField::Conductor),
                owned(&["Herbert von Karajan"]),
                "{name}"
            );
            let performer = if m4a {
                vec![]
            } else {
                owned(&["Glenn Gould (piano)", "Berliner Philharmoniker (orchestra)"])
            };
            assert_eq!(get(TagField::Performer), performer, "{name}");
            let musicians = vec![
                ("piano".to_owned(), "Glenn Gould".to_owned()),
                ("orchestra".to_owned(), "Berliner Philharmoniker".to_owned()),
            ];
            match index {
                0 => assert_eq!(vorbis_key_values(&file, "PERFORMER"), performer),
                1 => assert_eq!(id3_people(&file, "TMCL"), musicians),
                2 => assert_eq!(id3_people(&file, "IPLS"), musicians),
                _ => {}
            }
            assert_eq!(
                get(TagField::Work),
                owned(&[
                    "Concerto for Piano and Orchestra no. 3 in C minor, op. 37: I. Allegro con brio"
                ]),
                "{name}"
            );
            assert_eq!(
                get(TagField::MusicBrainzWorkId),
                owned(&["d6843235-961a-37ab-85a4-a0e7e027f752"]),
                "{name}"
            );
        } else if index < 8 {
            // ID3v2.3 holds several names in one frame, joined by "/".
            assert_eq!(
                get(TagField::Producer).join("/"),
                "Lady Gaga/RedOne",
                "{name}"
            );
            assert_eq!(
                get(TagField::Remixer),
                owned(&["The Sound of Arrows"]),
                "{name}"
            );
            assert_eq!(
                get(TagField::Composer).join("/"),
                "Lady Gaga/RedOne",
                "{name}"
            );
            assert_eq!(get(TagField::Work), owned(&["Alejandro"]), "{name}");
        } else {
            // "Release credits" style writes the work's writers as
            // credited there.
            let writers = [
                "Beyoncé Knowles",
                "LaShawn Daniels",
                "Lazonate Franklin",
                "Rodney Jerkins",
                "Lady Gaga",
            ];
            let (engineer, mixer) = ("Beyoncé", "Lady Gaga");
            match index {
                8 => {
                    assert_eq!(vorbis_key_values(&file, "ENGINEER"), owned(&[engineer]));
                    assert_eq!(vorbis_key_values(&file, "MIXER"), owned(&[mixer]));
                    assert_eq!(vorbis_key_values(&file, "WRITER"), owned(&writers));
                }
                9 | 10 => {
                    let (frame, writer) = if index == 9 {
                        ("TIPL", owned(&writers))
                    } else {
                        ("IPLS", vec![writers.join("/")])
                    };
                    let pairs = id3_people(&file, frame);
                    for pair in [("engineer", engineer), ("mix", mixer)] {
                        assert!(
                            pairs.contains(&(pair.0.to_owned(), pair.1.to_owned())),
                            "{name}: {pairs:?}"
                        );
                    }
                    assert_eq!(id3_txxx_values(&file, "Writer"), writer, "{name}");
                }
                _ => {
                    assert_eq!(mp4_freeform(&file, "ENGINEER"), owned(&[engineer]));
                    assert_eq!(mp4_freeform(&file, "MIXER"), owned(&[mixer]));
                    assert_eq!(mp4_freeform(&file, "writer"), owned(&writers));
                }
            }
            assert_eq!(get(TagField::Performer), owned(&[]), "{name}");
        }
    }

    // The credits are on the stored edition now; a second plan asks
    // nothing and finds nothing to change. ID3v2.3 holds several names
    // as one "/"-joined value; read back that way, it is no change.
    let (_, again) = managed
        .preview(SelectionKind::Albums, &[&album], "credits-again")
        .await;
    assert_eq!(scripted.lookups.lock().expect("calls").len(), 1);
    assert_eq!(again.len(), tracks.len());
    for item in &again {
        assert!(
            item.diff.field_mutations.is_empty(),
            "{:?}: {:?}",
            item.track_id,
            item.diff.field_mutations
        );
    }
}

/// While MusicBrainz is down an automatic look writes nothing and the
/// album waits, saying why; a look once it answers writes the credits by
/// itself. A composer edited by hand after that is the user's: the
/// worker marks the album and writes nothing.
#[tokio::test]
async fn automatic_credits_wait_for_musicbrainz_and_respect_a_hand_edit() {
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use droppedneedle_library::tags::{TagEdit, TagField, read_fields};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-credits-auto", |_| {}, scripted.clone()).await;
    let (album, tracks) = managed
        .add_album_from("first", &["management_full.flac"], |_| {})
        .await;
    managed.activate().await;
    let file = managed.path(&tracks[0]);
    let before = file_sha256(&file);
    // The dry run was a person's; the worker's lookups come after it.
    let asked_by_people = scripted.lookups.lock().expect("calls").len();

    managed.look(&album).await;
    assert_eq!(
        file_sha256(&file),
        before,
        "nothing written while it is down"
    );
    assert!(managed.automatic_runs().is_empty());
    let waiting: String = managed
        .db()
        .query_row(
            "SELECT last_reason_code FROM library_management_automatic_queue \
             WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("the album waits");
    assert_eq!(waiting, "METADATA_UNAVAILABLE");

    // However long MusicBrainz stays down, the album is never parked: the
    // worker keeps looking by itself.
    managed
        .db()
        .execute(
            "UPDATE library_management_automatic_queue SET attempts = 7, not_before = 0 \
             WHERE local_album_id = ?1",
            [&album],
        )
        .expect("many tries");
    managed.worker_looks().await;
    let (attempts, not_before): (i64, f64) = managed
        .db()
        .query_row(
            "SELECT attempts, not_before FROM library_management_automatic_queue \
             WHERE local_album_id = ?1",
            [&album],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the album still waits");
    assert_eq!(attempts, 8);
    assert!(not_before < 1.0e12, "not parked: {not_before}");

    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    droppedneedle_library::management::automatic::wake(
        &managed.db(),
        Some("METADATA_UNAVAILABLE"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64(),
    )
    .expect("woken");
    managed.worker_looks().await;
    let file = managed.path(&tracks[0]);
    assert_eq!(
        read_fields(&file).expect("tags read")[&TagField::Composer],
        vec!["Ludwig van Beethoven".to_owned()],
        "the credits are written once MusicBrainz answers"
    );
    assert!(
        scripted
            .lookups
            .lock()
            .expect("calls")
            .iter()
            .skip(asked_by_people)
            .all(|(_, background)| *background),
        "automatic lookups wait behind people's"
    );
    assert_eq!(managed.mark(&album), None);

    managed
        .edit_outside(
            &tracks[0],
            TagEdit::verbatim(TagField::Composer, vec!["By Hand".to_owned()]),
        )
        .await;
    let edited = file_sha256(&file);
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album).as_deref(), Some("files_changed"));
    assert_eq!(file_sha256(&file), edited, "nothing written");
}

/// Grafting fetched credits and artist names onto a stored edition keeps
/// everything else in it, every row it answers to, and when it was
/// fetched, so an old tracklist never reads as fresh.
#[tokio::test]
async fn grafted_credits_keep_the_stored_edition() {
    let (_scratch, library, _music) = bare_library("lib-graft-credits").await;
    let store = &library.identify_store;
    let release = Release {
        id: "rel-new".to_owned(),
        title: "Kept".to_owned(),
        old_ids: vec!["rel-old".to_owned()],
        tracks: vec![ReleaseTrack {
            id: "ed4f20f0-61cf-439d-9c64-1d9acb043cbb".to_owned(),
            recording_id: "33ea7e59-3691-4414-92d3-6bce48d5d9cc".to_owned(),
            title: "Allegro".to_owned(),
            artists: vec![CreditedArtist {
                id: "gould".to_owned(),
                name: "Glenn Gould".to_owned(),
                ..CreditedArtist::default()
            }],
            ..ReleaseTrack::default()
        }],
        ..Release::default()
    };
    store
        .save_release(&release, droppedneedle_kernel::db::Lane::Background)
        .unwrap();
    let db = droppedneedle_kernel::db::open_connection(&_scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let times = || -> Vec<(String, f64, Option<f64>)> {
        let mut statement = db
            .prepare(
                "SELECT entity_id, fetched_at, expires_at \
                 FROM library_management_metadata_snapshots ORDER BY entity_id",
            )
            .expect("prepared");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("rows")
            .collect::<rusqlite::Result<_>>()
            .expect("rows read")
    };
    let saved = times();
    assert_eq!(saved.len(), 2);

    let mut fetched = droppedneedle_library::matching::model::ReleaseRelations {
        fetched_at_ms: 42,
        ..Default::default()
    };
    fetched
        .tracks
        .insert(release.tracks[0].id.clone(), concerto_credits());
    fetched
        .tracks
        .insert("not-on-this-edition".to_owned(), remix_credits());
    let artist = |name: &str| droppedneedle_library::matching::model::ArtistName {
        name: name.to_owned(),
        ..Default::default()
    };
    let names = droppedneedle_library::matching::model::ArtistNames {
        fetched_at_ms: 43,
        artists: [
            ("gould".to_owned(), artist("Glenn Gould")),
            ("stranger".to_owned(), artist("Not credited here")),
        ]
        .into(),
    };
    let grafted = store
        .graft_extras(
            "rel-new",
            &droppedneedle_library::matching::model::ReleaseExtras {
                relations: Some(fetched),
                names: Some(names),
            },
        )
        .unwrap()
        .expect("grafted");
    assert_eq!(times(), saved, "fetched and expiry times are kept");
    for id in ["rel-new", "rel-old"] {
        let found = store.release(id, None).unwrap().expect("document found");
        assert_eq!(found, grafted);
        assert_eq!(found.title, "Kept");
        assert_eq!(found.old_ids, vec!["rel-old".to_owned()]);
        let relations = found.recording_relations.expect("credits stored");
        assert_eq!(relations.fetched_at_ms, 42);
        assert_eq!(
            relations.tracks.keys().collect::<Vec<_>>(),
            vec![&release.tracks[0].id],
            "only the edition's own tracks take credits"
        );
        let names = found.artist_names.expect("names stored");
        assert_eq!(names.fetched_at_ms, 43);
        assert_eq!(
            names.artists.keys().collect::<Vec<_>>(),
            vec!["gould"],
            "only artists the edition credits are kept"
        );
    }
}

/// Two editions, `R` and `R2`, of made-up album `n` titled `title`: the
/// same three recordings, each edition with its own track ids.
fn sibling_editions(n: u32, title: &str) -> (Release, Release) {
    const TITLES: [&str; 3] = ["Airbag", "Paranoid Android", "Subterranean Homesick Alien"];
    let edition = |letter: char| Release {
        id: format!("5e1a0000-000{n}-4000-8000-00000000000{letter}"),
        release_group_id: format!("5e1a0000-000{n}-4000-8000-0000000000ff"),
        title: title.to_owned(),
        artists: vec![CreditedArtist {
            id: "a74b1b7f-71a5-4011-9441-d0b5e4122711".to_owned(),
            name: "Radiohead".to_owned(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: TITLES
            .iter()
            .enumerate()
            .map(|(index, title)| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("5e1a0000-000{n}-4000-8000-0000000{letter}{index:04}"),
                recording_id: format!("5e1a0000-000{n}-4000-8000-0000000d{index:04}"),
                title: (*title).to_owned(),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: None,
            })
            .collect(),
        ..Release::default()
    };
    (edition('a'), edition('b'))
}

/// One file per track of `release` in `folder`, tagged as that release
/// with its recordings (when `recordings`) and the given track ids.
fn plant_edition(
    music: &Path,
    folder: &str,
    album: &str,
    release: &Release,
    track_ids: &[String],
    recordings: bool,
) {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    for (index, track) in release.tracks.iter().enumerate() {
        let file = plant(
            music,
            &format!("{folder}/{:02}.flac", index + 1),
            "flac_full_01.flac",
        );
        let recording = if recordings {
            vec![track.recording_id.clone()]
        } else {
            Vec::new()
        };
        save_tags(
            &file,
            &[
                TagEdit::verbatim(TagField::Title, vec![track.title.clone()]),
                TagEdit::verbatim(TagField::Album, vec![album.to_owned()]),
                TagEdit::verbatim(TagField::TrackNumber, vec![(index + 1).to_string()]),
                TagEdit::verbatim(TagField::MusicBrainzReleaseId, vec![release.id.clone()]),
                TagEdit::verbatim(
                    TagField::MusicBrainzReleaseGroupId,
                    vec![release.release_group_id.clone()],
                ),
                TagEdit::verbatim(TagField::MusicBrainzRecordingId, recording),
                TagEdit::verbatim(
                    TagField::MusicBrainzReleaseTrackId,
                    vec![track_ids[index].clone()],
                ),
            ],
        )
        .expect("file tagged");
    }
}

/// The track ids of `release`, with track 2's taken from `other`.
fn stray_second(release: &Release, other: &Release) -> Vec<String> {
    let mut ids: Vec<String> = release
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect();
    ids[1] = other.tracks[1].id.clone();
    ids
}

/// A file whose release and recording ids name edition R and whose track
/// id is R2's is matched to R by its recording: the plan says so, and the
/// Picard profile writes R's track id into it, after which the plan has
/// nothing to say. A file naming R with R2's track id and no recording id
/// is not placed on R, and the plan says why.
#[tokio::test]
async fn a_track_id_from_another_edition_is_corrected() {
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::operations::plan::EditionPlan;
    use droppedneedle_library::tags::TagField;

    let managed = ManagedLibrary::start("lib-stray-track-id", |_| {}).await;
    let library = &managed.library;
    let (r, r2) = sibling_editions(1, "Stray Ids");
    plant_edition(
        &managed.music,
        "stray",
        "Stray Ids",
        &r,
        &stray_second(&r, &r2),
        true,
    );
    rescan(library).await;
    let tracks: Vec<String> = (1..=3)
        .map(|n| {
            library
                .scan_store
                .track_at("music", &format!("stray/{n:02}.flac"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album = library
        .scan_store
        .album_for_track(&tracks[0])
        .unwrap()
        .expect("album");
    let scripted = &managed.scratch.identify;
    scripted.set_recall(RecallResult {
        releases: vec![r.clone(), r2.clone()],
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 1);
    let identity = library
        .identify_store
        .album_identity(&album)
        .unwrap()
        .expect("identity");
    assert_eq!(identity.release_mbid.as_deref(), Some(r.id.as_str()));

    let ops = crate::common::library::operations(library);
    let notes = |plan: &EditionPlan| -> Vec<(String, &'static str, Option<&'static str>)> {
        let mut notes: Vec<_> = plan
            .files
            .iter()
            .map(|file| {
                (
                    file.local_track_id.clone(),
                    file.fate.as_str(),
                    file.note.map(|note| note.code),
                )
            })
            .collect();
        notes.sort();
        notes
    };
    let mut expected = vec![
        (tracks[0].clone(), "keep", None),
        (tracks[1].clone(), "keep", Some("TRACK_ID_CORRECTED")),
        (tracks[2].clone(), "keep", None),
    ];
    expected.sort();
    let plan = ops.edition_plan(&album, Some(&r.id)).await.expect("plan");
    assert_eq!(notes(&plan), expected);

    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&album], "stray-ids")
        .await;
    assert!(
        items.iter().all(|item| item.eligibility.writes()),
        "{:?}",
        items.iter().map(|item| &item.facts).collect::<Vec<_>>()
    );
    let applied = management_apply(library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    assert_eq!(
        native_values(
            &managed.path(&tracks[1]),
            TagField::MusicBrainzReleaseTrackId
        ),
        vec![r.tracks[1].id.clone()]
    );
    rescan(library).await;
    let plan = ops.edition_plan(&album, Some(&r.id)).await.expect("plan");
    assert!(
        plan.files.iter().all(|file| file.note.is_none()),
        "{plan:?}"
    );

    // One file naming R with R2's track id and nothing to match it by: the
    // album is contradictory, and the plan of R says why the file is off it.
    let mut lone = r.clone();
    lone.tracks.truncate(2);
    lone.tracks.remove(0);
    let ids = vec![r2.tracks[1].id.clone()];
    plant_edition(
        &managed.music,
        "lone",
        "Stray Ids Extra",
        &lone,
        &ids,
        false,
    );
    rescan(library).await;
    let track = library
        .scan_store
        .track_at("music", "lone/01.flac")
        .unwrap()
        .expect("track indexed");
    let lone_album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    scripted.set_recall(RecallResult {
        releases: vec![r.clone()],
        ..RecallResult::default()
    });
    library.identification.identify_tick().await;
    assert!(
        library
            .identify_store
            .album_identity(&lone_album)
            .unwrap()
            .is_none()
    );
    let plan = ops
        .edition_plan(&lone_album, Some(&r.id))
        .await
        .expect("plan");
    let file = &plan.files[0];
    assert_ne!(file.fate.as_str(), "keep", "{plan:?}");
    assert_eq!(file.note.map(|note| note.code), Some("TRACK_ID_ELSEWHERE"));
}

/// What the migration tool leaves after committing a v2 import. The
/// one-time re-checks run only on such a database.
fn carried_from_v2(db: &rusqlite::Connection) {
    db.execute(
        "INSERT INTO import_runs (id, instance_id, exported_at, exit_code, \
         entity_counts, applied_at) VALUES ('import-1', 'v2-instance', \
         '2026-01-01T00:00:00Z', 'OK', '{}', '2026-01-01T00:00:01Z')",
        [],
    )
    .expect("import recorded");
}

/// After an upgrade, albums the old rule may have left on the wrong
/// edition because one file's track id named another are identified once
/// more, by themselves: an automatic identity on another edition (A)
/// moves to the edition its files name, keeping its album and tracks, and
/// the edition it replaced can be undone. A person's edition (B), a match
/// a person took off (C) and an album already on its files' edition (D)
/// are left alone. It happens once.
#[tokio::test]
async fn albums_an_older_matcher_misplaced_are_looked_at_once() {
    let (scratch, library, music) = bare_library("lib-track-id-relook").await;
    let editions: Vec<(Release, Release)> = (1..=4)
        .map(|n| sibling_editions(n, &format!("Relook {n}")))
        .collect();
    for (index, (r, r2)) in editions.iter().enumerate() {
        let ids = if index == 3 {
            r.tracks.iter().map(|track| track.id.clone()).collect()
        } else {
            stray_second(r, r2)
        };
        let folder = format!("album-{index}");
        plant_edition(&music, &folder, &r.title, r, &ids, true);
    }
    add_music_root(&library, &music).await;
    let album = |index: usize| {
        let track = library
            .scan_store
            .track_at("music", &format!("album-{index}/01.flac"))
            .unwrap()
            .expect("track indexed");
        library
            .scan_store
            .album_for_track(&track)
            .unwrap()
            .expect("album")
    };
    let (a, b, c, d) = (album(0), album(1), album(2), album(3));
    // C's editions are never offered, so it ends unmatched.
    let releases = editions
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 2)
        .flat_map(|(_, (r, r2))| [r.clone(), r2.clone()])
        .collect();
    scratch.identify.set_recall(RecallResult {
        releases,
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 4);

    // What the older matcher and people left behind.
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let on_r2 = |album: &str, r2: &Release, source: &str| {
        db.execute(
            "UPDATE local_album_external_identities SET release_mbid = ?2, \
             decision_source = ?3 WHERE local_album_id = ?1",
            rusqlite::params![album, r2.id, source],
        )
        .expect("identity moved");
        db.execute(
            "UPDATE local_track_external_identities SET release_mbid = ?2, \
             release_track_mbid = replace(release_track_mbid, '-0000000a', '-0000000b') \
             WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            rusqlite::params![album, r2.id],
        )
        .expect("tracks moved");
    };
    on_r2(&a, &editions[0].1, "automatic");
    on_r2(&b, &editions[1].1, "automatic");
    // The older matcher left C unmatched, its edition vetoed by the stray
    // track id; that release's document has since aged out.
    let (c_release, c_other) = &editions[2];
    let c_tracks: Vec<String> = {
        let mut stmt = db
            .prepare("SELECT id FROM local_tracks WHERE local_album_id = ?1 ORDER BY track_number")
            .expect("tracks read");
        stmt.query_map([&c], |row| row.get(0))
            .expect("tracks read")
            .collect::<Result<_, _>>()
            .expect("tracks read")
    };
    let vetoed = json!([{
        "candidate_key": c_release.id,
        "release_group_mbid": c_release.release_group_id,
        "release_mbid": c_release.id,
        "album_title": c_release.title,
        "album_artist_name": "Radiohead",
        "track_evidence": c_tracks.iter().enumerate().map(|(index, track)| {
            let stray = index == 1;
            json!({
                "local_track_id": track,
                "classification": if stray { "contradictory" } else { "supported" },
                "evidence_kinds": [if stray { "release_track_mbid_conflict" } else { "embedded_id" }],
                "recording_mbid": (!stray).then(|| c_release.tracks[index].recording_id.clone()),
                "release_track_mbid": (!stray).then(|| c_release.tracks[index].id.clone()),
            })
        }).collect::<Vec<_>>(),
        "score": 0.0,
        "reason_code": "CONFLICTING_TRACK_EVIDENCE",
    }]);
    assert_ne!(c_other.tracks[1].id, c_release.tracks[1].id);
    db.execute(
        "INSERT OR REPLACE INTO library_album_match_state \
         (local_album_id, state, reason_code, candidates_json, updated_at) \
         VALUES (?1, 'unmatched', 'CONFLICTING_TRACK_EVIDENCE', ?2, 0)",
        rusqlite::params![c, vetoed.to_string()],
    )
    .expect("old verdict planted");
    let sorted = |mut albums: Vec<String>| {
        albums.sort();
        albums
    };
    let set =
        || droppedneedle_library::repairs::store::track_id_backfill_albums(&db).expect("set reads");
    assert_eq!(set(), sorted(vec![a.clone(), b.clone(), c.clone()]));
    on_r2(&b, &editions[1].1, "manual");
    db.execute(
        "UPDATE library_album_match_state SET reason_code = 'DETACHED_BY_PERSON' \
         WHERE local_album_id = ?1",
        [&c],
    )
    .expect("match taken off");
    let track_ids = |album: &str| -> Vec<String> {
        let mut stmt = db
            .prepare("SELECT id FROM local_tracks WHERE local_album_id = ?1 ORDER BY id")
            .expect("tracks read");
        stmt.query_map([album], |row| row.get(0))
            .expect("tracks read")
            .collect::<Result<_, _>>()
            .expect("tracks read")
    };
    assert_eq!(set(), vec![a.clone()]);
    let a_tracks = track_ids(&a);
    let queued = || -> Vec<String> {
        let mut stmt = db
            .prepare(
                "SELECT local_album_id FROM library_identify_jobs \
                 WHERE state = 'queued' ORDER BY local_album_id",
            )
            .expect("queue reads");
        stmt.query_map([], |row| row.get(0))
            .expect("queue reads")
            .collect::<Result<_, _>>()
            .expect("queue reads")
    };
    assert!(queued().is_empty());

    carried_from_v2(&db);
    let repairs = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&repairs, "worker").await;
    assert_eq!(queued(), vec![a.clone()]);
    assert_eq!(library.identification.identify_tick().await, 1);

    let release_of = |album: &str| {
        library
            .identify_store
            .album_identity(album)
            .unwrap()
            .map(|identity| {
                (
                    identity.release_mbid,
                    identity.decision_source.as_str().to_owned(),
                )
            })
    };
    let named =
        |index: usize, source: &str| Some((Some(editions[index].0.id.clone()), source.to_owned()));
    assert_eq!(release_of(&a), named(0, "automatic"));
    assert_eq!(
        release_of(&b),
        Some((Some(editions[1].1.id.clone()), "manual".to_owned()))
    );
    assert_eq!(release_of(&c), None);
    assert_eq!(release_of(&d), named(3, "automatic"));
    assert_eq!(track_ids(&a), a_tracks);
    let undo: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_automatic_edition_undo \
             WHERE local_album_id = ?1 AND consumed_at IS NULL",
            [&a],
            |row| row.get(0),
        )
        .expect("undo reads");
    assert_eq!(undo, 1, "the replaced edition can be undone");

    // The next start looks no more.
    let restarted = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&restarted, "worker").await;
    assert!(queued().is_empty());
}

/// v2 carried some matches with a release group and no edition. Until it
/// has one, the album's status says the edition is not chosen yet. After
/// the upgrade such a match the app made (A) is identified once more, by
/// itself, and lands on the edition its files fit, which can be undone. A
/// group a person chose, now (B) or in v2 (C), is left alone. It happens
/// once per database.
#[tokio::test]
async fn albums_matched_to_a_group_only_get_an_edition_once() {
    use droppedneedle_library::operations::status::edition_status;

    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let (scratch, library, music) = bare_library("lib-group-only-relook").await;
    let editions: Vec<Release> = (1..=3)
        .map(|n| sibling_editions(n, &format!("Group Only {n}")).0)
        .collect();
    // Files tagged with their release group and recordings, no release.
    for (index, release) in editions.iter().enumerate() {
        for (number, track) in release.tracks.iter().enumerate() {
            let file = plant(
                &music,
                &format!("album-{index}/{:02}.flac", number + 1),
                "flac_full_01.flac",
            );
            save_tags(
                &file,
                &[
                    TagEdit::verbatim(TagField::Title, vec![track.title.clone()]),
                    TagEdit::verbatim(TagField::Album, vec![release.title.clone()]),
                    TagEdit::verbatim(TagField::Artist, vec!["Radiohead".to_owned()]),
                    TagEdit::verbatim(TagField::AlbumArtist, vec!["Radiohead".to_owned()]),
                    TagEdit::verbatim(TagField::TrackNumber, vec![(number + 1).to_string()]),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseId, Vec::new()),
                    TagEdit::verbatim(
                        TagField::MusicBrainzReleaseGroupId,
                        vec![release.release_group_id.clone()],
                    ),
                    TagEdit::verbatim(
                        TagField::MusicBrainzRecordingId,
                        vec![track.recording_id.clone()],
                    ),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
                ],
            )
            .expect("file tagged");
        }
    }
    add_music_root(&library, &music).await;
    let album = |index: usize| {
        let track = library
            .scan_store
            .track_at("music", &format!("album-{index}/01.flac"))
            .unwrap()
            .expect("track indexed");
        library
            .scan_store
            .album_for_track(&track)
            .unwrap()
            .expect("album")
    };
    let (a, b, c) = (album(0), album(1), album(2));
    scratch.identify.set_recall(RecallResult {
        releases: editions.clone(),
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 3);

    // What v2 carried: the group only, with no track matches.
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let group_only = |album: &str, source: &str| {
        db.execute(
            "UPDATE local_album_external_identities SET release_mbid = NULL, \
             decision_source = ?2 WHERE local_album_id = ?1",
            rusqlite::params![album, source],
        )
        .expect("identity carried");
        db.execute(
            "DELETE FROM local_track_external_identities WHERE local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            [album],
        )
        .expect("tracks carried");
    };
    group_only(&a, "embedded");
    group_only(&b, "manual");
    group_only(&c, "legacy_import");
    let reason = |album: &str| {
        let status = edition_status(&db, album)
            .expect("status reads")
            .expect("album known");
        (status.reason.code, status.reason.message)
    };
    let (code, message) = reason(&a);
    assert_eq!(code, "EDITION_NOT_CHOSEN");
    assert!(message.starts_with("Edition not chosen yet"), "{message}");
    assert_eq!(reason(&b).0, "CHOSEN");
    assert_eq!(reason(&c).0, "CHOSEN");
    let queued = || -> Vec<String> {
        let mut stmt = db
            .prepare(
                "SELECT local_album_id FROM library_identify_jobs \
                 WHERE state = 'queued' ORDER BY local_album_id",
            )
            .expect("queue reads");
        stmt.query_map([], |row| row.get(0))
            .expect("queue reads")
            .collect::<Result<_, _>>()
            .expect("queue reads")
    };
    assert!(queued().is_empty());

    // A database no v2 import was committed into has nothing to re-check.
    droppedneedle_library::repairs::tick(
        &crate::common::library::repair_worker(&library),
        "worker",
    )
    .await;
    assert!(queued().is_empty());
    let marked: bool = db
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM library_operation_jobs \
             WHERE idempotency_key = 'group-only-edition:v1:backfill')",
            [],
            |row| row.get(0),
        )
        .expect("marker reads");
    assert!(!marked, "nothing is marked done");
    carried_from_v2(&db);
    let repairs = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&repairs, "worker").await;
    assert_eq!(queued(), vec![a.clone()]);
    assert_eq!(library.identification.identify_tick().await, 1);

    let release_of = |album: &str| {
        library
            .identify_store
            .album_identity(album)
            .unwrap()
            .map(|identity| {
                (
                    identity.release_mbid,
                    identity.decision_source.as_str().to_owned(),
                )
            })
    };
    assert_eq!(
        release_of(&a),
        Some((Some(editions[0].id.clone()), "automatic".to_owned()))
    );
    assert_eq!(release_of(&b), Some((None, "manual".to_owned())));
    assert_eq!(release_of(&c), Some((None, "legacy_import".to_owned())));
    assert_eq!(reason(&a).0, "SUPPORTED");
    let undo: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_automatic_edition_undo \
             WHERE local_album_id = ?1 AND consumed_at IS NULL",
            [&a],
            |row| row.get(0),
        )
        .expect("undo reads");
    assert_eq!(undo, 1, "the edition it got can be undone");

    // The next start looks no more, even at a group-only match.
    group_only(&a, "embedded");
    let restarted = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&restarted, "worker").await;
    assert!(queued().is_empty());
}

/// Before matching read the release group the files' tags name, an album
/// whose files all name one group could be confirmed on a release of
/// another. After the upgrade such a match the app made (A) is identified
/// once more, by itself, and comes back as a guess saying the files name
/// another group. The same shape a person chose, now (B) or in v2 (C), is
/// left alone. It happens once per database.
#[tokio::test]
async fn albums_confirmed_before_the_newer_rules_are_checked_once() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    const NAMED_GROUP: &str = "5e1a0000-0009-4000-8000-0000000000ee";
    let (scratch, library, music) = bare_library("lib-older-confirmations").await;
    let editions: Vec<Release> = (1..=3)
        .map(|n| sibling_editions(n, &format!("Older {n}")).0)
        .collect();
    for (index, release) in editions.iter().enumerate() {
        for (number, track) in release.tracks.iter().enumerate() {
            let file = plant(
                &music,
                &format!("album-{index}/{:02}.flac", number + 1),
                "flac_full_01.flac",
            );
            save_tags(
                &file,
                &[
                    TagEdit::verbatim(TagField::Title, vec![track.title.clone()]),
                    TagEdit::verbatim(TagField::Album, vec![release.title.clone()]),
                    TagEdit::verbatim(TagField::Artist, vec!["Radiohead".to_owned()]),
                    TagEdit::verbatim(TagField::AlbumArtist, vec!["Radiohead".to_owned()]),
                    TagEdit::verbatim(TagField::TrackNumber, vec![(number + 1).to_string()]),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseId, Vec::new()),
                    TagEdit::verbatim(
                        TagField::MusicBrainzReleaseGroupId,
                        vec![NAMED_GROUP.to_owned()],
                    ),
                    TagEdit::verbatim(
                        TagField::MusicBrainzRecordingId,
                        vec![track.recording_id.clone()],
                    ),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
                ],
            )
            .expect("file tagged");
        }
    }
    add_music_root(&library, &music).await;
    let album = |index: usize| {
        let track = library
            .scan_store
            .track_at("music", &format!("album-{index}/01.flac"))
            .unwrap()
            .expect("track indexed");
        library
            .scan_store
            .album_for_track(&track)
            .unwrap()
            .expect("album")
    };
    let (a, b, c) = (album(0), album(1), album(2));
    scratch.identify.set_recall(RecallResult {
        releases: editions.clone(),
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 3);

    // What an older matcher left: every match confirmed outside the group
    // the files name, B chosen by a person and C carried from v2.
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    db.execute("DELETE FROM library_album_match_state", [])
        .expect("older confirmation planted");
    for (album, source) in [(&b, "manual"), (&c, "legacy_import")] {
        db.execute(
            "UPDATE local_album_external_identities SET decision_source = ?2 \
             WHERE local_album_id = ?1",
            rusqlite::params![album, source],
        )
        .expect("person's match planted");
    }
    let queued = || -> Vec<String> {
        let mut stmt = db
            .prepare(
                "SELECT local_album_id FROM library_identify_jobs \
                 WHERE state = 'queued' ORDER BY local_album_id",
            )
            .expect("queue reads");
        stmt.query_map([], |row| row.get(0))
            .expect("queue reads")
            .collect::<Result<_, _>>()
            .expect("queue reads")
    };
    assert!(queued().is_empty());

    carried_from_v2(&db);
    let repairs = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&repairs, "worker").await;
    assert_eq!(queued(), vec![a.clone()]);
    assert_eq!(library.identification.identify_tick().await, 1);

    let flag = |album: &str| {
        library
            .identify_store
            .match_flag(album)
            .unwrap()
            .map(|flag| flag.reason_code)
    };
    assert_eq!(flag(&a), Some("NAMED_GROUP_DIFFERS".to_owned()));
    let identity = library
        .identify_store
        .album_identity(&a)
        .unwrap()
        .expect("guess kept");
    assert_eq!(
        (identity.release_mbid, identity.decision_source.as_str()),
        (Some(editions[0].id.clone()), "automatic")
    );
    assert_eq!(flag(&b), None);
    assert_eq!(flag(&c), None);

    // The next start looks no more.
    db.execute("DELETE FROM library_album_match_state", [])
        .expect("older confirmation planted");
    let restarted = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&restarted, "worker").await;
    assert!(queued().is_empty());
}

/// An album the app confirmed on a vinyl edition while its files' barcode,
/// catalog number, media and country are the CD's, and no file carries a
/// MusicBrainz release ID (A), is identified once more after the upgrade,
/// but only once its files' edition tags have been read, and moves to the
/// CD. The move can be undone, and the undone album is not moved again.
/// The same files carrying the vinyl's release ID (B) keep the vinyl: the
/// release ID outranks the barcode.
#[tokio::test]
async fn an_album_moves_to_the_edition_its_barcode_names_once() {
    use droppedneedle_library::identify::models::IdentifyKind;
    use droppedneedle_library::matching::ReleaseMedium;
    use droppedneedle_library::operations::decisions::{
        live_automatic_edition_undo, undo_automatic_edition,
    };
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let (scratch, library, music) = bare_library("lib-edition-ids").await;
    let (mut vinyl, mut cd) = sibling_editions(4, "Astral Weeks");
    for (release, media, country, barcode, catalog) in [
        (&mut vinyl, "12\" Vinyl", "GB", "5012345678900", "WS 1768"),
        (&mut cd, "CD", "US", "075992717625", "W2 1768"),
    ] {
        release.media = vec![ReleaseMedium {
            position: 1,
            format: Some(media.to_owned()),
            title: None,
            track_count: 3,
        }];
        release.country = Some(country.to_owned());
        release.barcode = Some(barcode.to_owned());
        release.catalog_numbers = vec![catalog.to_owned()];
    }
    for (folder, release_id) in [("album-a", ""), ("album-b", vinyl.id.as_str())] {
        for (number, track) in vinyl.tracks.iter().enumerate() {
            let file = plant(
                &music,
                &format!("{folder}/{:02}.flac", number + 1),
                "flac_full_01.flac",
            );
            let tag = |field, value: &str| TagEdit::verbatim(field, vec![value.to_owned()]);
            save_tags(
                &file,
                &[
                    tag(TagField::Title, &track.title),
                    tag(TagField::Album, "Astral Weeks"),
                    tag(TagField::Artist, "Radiohead"),
                    tag(TagField::AlbumArtist, "Radiohead"),
                    tag(TagField::TrackNumber, &(number + 1).to_string()),
                    tag(TagField::MusicBrainzRecordingId, &track.recording_id),
                    tag(TagField::MusicBrainzReleaseGroupId, &vinyl.release_group_id),
                    TagEdit::verbatim(
                        TagField::MusicBrainzReleaseId,
                        [release_id]
                            .into_iter()
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    ),
                    TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
                    tag(TagField::Media, "CD"),
                    tag(TagField::ReleaseCountry, "US"),
                    tag(TagField::Barcode, "075992717625"),
                    tag(TagField::CatalogNumber, "W2 1768"),
                ],
            )
            .expect("file tagged");
        }
    }
    add_music_root(&library, &music).await;
    let album = |folder: &str| {
        let track = library
            .scan_store
            .track_at("music", &format!("{folder}/01.flac"))
            .unwrap()
            .expect("track indexed");
        library
            .scan_store
            .album_for_track(&track)
            .unwrap()
            .expect("album")
    };
    let (a, b) = (album("album-a"), album("album-b"));
    // What an older matcher left: both confirmed on the vinyl.
    scratch.identify.set_recall(RecallResult {
        releases: vec![vinyl.clone()],
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 2);
    scratch.identify.set_recall(RecallResult {
        releases: vec![vinyl.clone(), cd.clone()],
        ..RecallResult::default()
    });
    let release_of = |album: &str| {
        library
            .identify_store
            .album_identity(album)
            .unwrap()
            .and_then(|identity| identity.release_mbid)
    };
    assert_eq!(release_of(&a), Some(vinyl.id.clone()));
    assert_eq!(release_of(&b), Some(vinyl.id.clone()));

    let mut db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let queued = |db: &rusqlite::Connection| -> Vec<String> {
        let mut stmt = db
            .prepare(
                "SELECT local_album_id FROM library_identify_jobs \
                 WHERE state = 'queued' ORDER BY local_album_id",
            )
            .expect("queue reads");
        stmt.query_map([], |row| row.get(0))
            .expect("queue reads")
            .collect::<Result<_, _>>()
            .expect("queue reads")
    };
    let marked = |db: &rusqlite::Connection| -> bool {
        db.query_row(
            "SELECT EXISTS (SELECT 1 FROM library_operation_jobs \
             WHERE idempotency_key = 'edition-ids:v1:backfill')",
            [],
            |row| row.get(0),
        )
        .expect("marker reads")
    };

    carried_from_v2(&db);
    // A file whose edition tags the scan has not read yet holds the pass.
    let first_track = library
        .scan_store
        .track_at("music", "album-a/01.flac")
        .unwrap()
        .expect("track indexed");
    let unread: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = db
        .query_row(
            "DELETE FROM local_track_edition_tags WHERE local_track_id = ?1 \
             RETURNING media, barcode, catalog_number, release_country",
            [&first_track],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("edition tags unread");
    droppedneedle_library::repairs::tick(
        &crate::common::library::repair_worker(&library),
        "worker",
    )
    .await;
    assert!(queued(&db).is_empty());
    assert!(!marked(&db), "the pass waits for the edition tags");

    db.execute(
        "INSERT INTO local_track_edition_tags (local_track_id, media, barcode, \
         catalog_number, release_country) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![first_track, unread.0, unread.1, unread.2, unread.3],
    )
    .expect("edition tags read");
    droppedneedle_library::repairs::tick(
        &crate::common::library::repair_worker(&library),
        "worker",
    )
    .await;
    assert_eq!(queued(&db), vec![a.clone()]);
    assert!(marked(&db));
    assert_eq!(library.identification.identify_tick().await, 1);
    assert_eq!(release_of(&a), Some(cd.id.clone()));
    assert_eq!(release_of(&b), Some(vinyl.id.clone()));
    assert_eq!(
        library.identify_store.match_flag(&a).unwrap(),
        None,
        "the move is a confirmed edition"
    );

    // An administrator undoes it; identified again, it keeps the vinyl.
    let (album_revision, identity_revision) = live_automatic_edition_undo(&db, &a)
        .expect("undo reads")
        .expect("the move can be undone");
    let tx = db.transaction().expect("transaction opens");
    undo_automatic_edition(&tx, &a, album_revision, identity_revision, "admin", 1.0)
        .expect("undone");
    tx.commit().expect("undo lands");
    assert_eq!(release_of(&a), Some(vinyl.id.clone()));
    library
        .identify
        .enqueue_album(
            "again",
            &a,
            IdentifyKind::Automatic,
            "rev-again",
            None,
            0,
            droppedneedle_kernel::db::Lane::Background,
        )
        .expect("queued");
    assert_eq!(library.identification.identify_tick().await, 1);
    assert_eq!(release_of(&a), Some(vinyl.id.clone()));

    // The next start looks no more.
    droppedneedle_library::repairs::tick(
        &crate::common::library::repair_worker(&library),
        "worker",
    )
    .await;
    assert!(queued(&db).is_empty());
}

/// Every album identification finishes with gets its catalog cleanup
/// look: the ones finished before the repairs worker started (a restart)
/// and every one of a window holding more than a page of them. The place
/// the cleanup reached is kept, so a restart looks at none of them again.
#[tokio::test]
async fn every_identified_album_gets_its_cleanup_across_restarts() {
    let (scratch, library, music) = bare_library("lib-identified-cleanup").await;
    plant(&music, "Artist/Album/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    // The one-time cleanup already ran on this database.
    db.execute(
        "INSERT INTO library_operation_jobs (id, kind, state, idempotency_key, created_at, \
         updated_at) VALUES ('backfill', 'repair', 'succeeded', ?1, 0, 0)",
        [droppedneedle_library::repairs::store::HYGIENE_BACKFILL_KEY],
    )
    .expect("one-time cleanup marked");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    // `count` empty album rows, each identified at `from_ms` plus its
    // number.
    let identified = |prefix: &str, count: i64, from_ms: i64| {
        db.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2) \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
             created_at, updated_at) SELECT printf('%s-%03d', ?1, n.i), a.root_id, \
             printf('%s-%03d', ?1, n.i), a.title, a.title_folded, a.album_artist_name, \
             a.album_artist_name_folded, a.album_artist_id, 'automatic', 0, 0 \
             FROM n, (SELECT * FROM local_albums LIMIT 1) a",
            rusqlite::params![prefix, count],
        )
        .expect("empty albums");
        db.execute(
            "INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
             input_revision, idempotency_key, created_ms, updated_ms) \
             SELECT 'job-' || id, id, 'automatic', 0, 'succeeded', 'r', 'key-' || id, ?2, \
             ?2 + CAST(substr(id, length(?1) + 2) AS INTEGER) \
             FROM local_albums WHERE id LIKE ?1 || '-%'",
            rusqlite::params![prefix, from_ms],
        )
        .expect("identified");
    };
    let looked_at = || -> (i64, i64) {
        db.query_row(
            "SELECT COUNT(DISTINCT w.local_album_id), COUNT(*) FROM library_operation_work w \
             JOIN library_operation_jobs j ON j.id = w.job_id \
             WHERE j.idempotency_key LIKE 'catalog-identity-hygiene:v1:identified:%' \
             AND (w.local_album_id LIKE 'before-%' OR w.local_album_id LIKE 'after-%')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("work reads")
    };
    let drain = |worker: droppedneedle_library::repairs::service::RepairWorker| async move {
        for _ in 0..20 {
            if !droppedneedle_library::repairs::tick(&worker, "worker").await {
                return;
            }
        }
        panic!("the repairs worker never ran out of work");
    };

    identified("before", 250, now_ms - 60_000);
    let worker = crate::common::library::repair_worker(&library);
    identified("after", 250, now_ms + 60_000);
    drain(worker).await;
    assert_eq!(looked_at(), (500, 500), "every identified album, once");

    drain(crate::common::library::repair_worker(&library)).await;
    assert_eq!(looked_at(), (500, 500), "a restart queues none again");
}

/// An album row an earlier version left on its own "Unknown Artist" row
/// (not the catalog's sentinel) while every file names the artist keeps
/// it through scans, because its files never change. After an upgrade it
/// takes the artist its tracks are credited to, by itself and once: the
/// change is in the audit, no artist row is merged or retired, and the
/// album is looked up again under the artist's name. A locked grouping is
/// left alone. The album's history lists the change, and a curator can
/// undo it while the album still holds what it gave: the album's earlier
/// artist, sort name and credits come back, and later cleanups leave it
/// there.
#[tokio::test]
async fn an_album_left_on_the_placeholder_artist_takes_its_tracks_artist() {
    use droppedneedle_library::http::history::album_history_view;
    use droppedneedle_library::operations::models::OperationError;
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let (scratch, library, music) = bare_library("lib-placeholder-artist").await;
    for (number, title) in [(1, "Close To The Edge"), (2, "And You And I")] {
        let file = plant(
            &music,
            &format!("Yes/1972 - Close To The Edge/{number:02}.flac"),
            "flac_full_01.flac",
        );
        save_tags(
            &file,
            &[
                TagEdit::verbatim(TagField::Title, vec![title.to_owned()]),
                TagEdit::verbatim(TagField::Album, vec!["Close To The Edge".to_owned()]),
                TagEdit::verbatim(TagField::Artist, vec!["Yes".to_owned()]),
                TagEdit::verbatim(TagField::Artists, Vec::new()),
                TagEdit::verbatim(TagField::AlbumArtist, Vec::new()),
                TagEdit::verbatim(TagField::TrackNumber, vec![number.to_string()]),
            ],
        )
        .expect("file tagged");
    }
    add_music_root(&library, &music).await;
    assert_eq!(library.identification.identify_tick().await, 1);
    let db = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let read = |sql: &str| -> Vec<String> {
        let mut stmt = db.prepare(sql).expect("query prepares");
        stmt.query_map([], |row| row.get(0))
            .expect("query runs")
            .collect::<Result<_, _>>()
            .expect("rows read")
    };
    let [album] = read("SELECT id FROM local_albums WHERE retired_into_album_id IS NULL")
        .try_into()
        .expect("one album");
    let track_credits = "SELECT DISTINCT local_artist_id || ' ' || credited_name \
                         FROM local_track_artists";
    let album_credits = "SELECT local_artist_id || ' ' || credited_name || ' ' || role \
                         FROM local_album_artists ORDER BY position";
    let [yes] = read(track_credits).try_into().expect("one track artist");

    // What the earlier version left behind.
    db.execute_batch(&format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('old-unknown', 'Unknown Artist', 'unknown artist', 'group', 0, 0); \
         UPDATE local_albums SET album_artist_id = 'old-unknown', \
         album_artist_name = 'Unknown Artist', album_artist_name_folded = 'unknown artist', \
         album_artist_sort_name = 'Artist, Unknown' WHERE id = '{album}'; \
         UPDATE local_album_artists SET local_artist_id = 'old-unknown', \
         credited_name = 'Unknown Artist', role = 'primary' WHERE local_album_id = '{album}';"
    ))
    .expect("the placeholder is planted");
    rescan(&library).await;
    assert_eq!(
        read("SELECT album_artist_name FROM local_albums"),
        ["Unknown Artist"],
        "a scan of unchanged files leaves the row as it is"
    );

    // A grouping a person locked is left as it is.
    let lock = |locked: bool| {
        db.execute(
            "UPDATE local_albums SET grouping_locked = ?2 WHERE id = ?1",
            rusqlite::params![album, locked],
        )
        .expect("lock set");
    };
    lock(true);
    let locked = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&locked, "worker").await;
    assert_eq!(
        read("SELECT album_artist_name FROM local_albums"),
        ["Unknown Artist"],
        "a locked album keeps its artist"
    );
    lock(false);

    let repairs = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&repairs, "worker").await;
    assert_eq!(
        read("SELECT album_artist_name || ' ' || album_artist_id FROM local_albums"),
        [format!("Yes {}", yes.split(' ').next().expect("artist id"))]
    );
    assert_eq!(read(album_credits), [format!("{yes} main")]);
    assert_eq!(
        read(
            "SELECT reason_code || ' ' || json_extract(before_json, '$.album_artist_id') \
             FROM library_catalog_actions WHERE action_kind = 'album_artist'"
        ),
        ["PLACEHOLDER_ARTIST_FROM_TRACKS old-unknown"]
    );
    assert_eq!(
        read(
            "SELECT id FROM local_artists WHERE id = 'old-unknown' \
             AND retired_into_artist_id IS NULL"
        ),
        ["old-unknown"],
        "the placeholder row is not merged"
    );
    assert!(read("SELECT id FROM local_artist_merge_candidates").is_empty());
    assert_eq!(
        read("SELECT kind FROM library_identify_jobs WHERE state = 'queued'"),
        ["historical"]
    );

    // The next start finds nothing to do.
    let restarted = crate::common::library::repair_worker(&library);
    droppedneedle_library::repairs::tick(&restarted, "worker").await;
    assert_eq!(
        read(
            "SELECT CAST(COUNT(*) AS TEXT) FROM library_catalog_actions \
             WHERE action_kind = 'album_artist'"
        ),
        ["1"]
    );
    assert_eq!(
        read(
            "SELECT CAST(COUNT(*) AS TEXT) FROM library_operation_jobs \
             WHERE idempotency_key = 'catalog-identity-hygiene:v1:backfill'"
        ),
        ["1"]
    );

    // The album's history lists the change, with Undo while it applies.
    let ops = crate::common::library::operations(&library);
    let history = || -> Value {
        let view = album_history_view(&ops, &album, "curator").expect("history reads");
        serde_json::to_value(view).expect("history serializes")["items"].clone()
    };
    let [repair] = read(
        "SELECT id FROM library_catalog_actions \
         WHERE reason_code = 'PLACEHOLDER_ARTIST_FROM_TRACKS'",
    )
    .try_into()
    .expect("one repair");
    let items = history();
    assert_eq!(
        items,
        json!([{"kind": "artist_from_tracks", "at": items[0]["at"], "by": null,
                "track_count": 0, "track_titles": [], "other_album_title": null,
                "undo_id": repair}])
    );
    let undo = |id: &str| match ops.undo_artist_repair(id, "curator") {
        Ok(()) => "restored".to_owned(),
        Err(OperationError::NotFound(reason) | OperationError::Conflict(reason)) => {
            reason.code.to_owned()
        }
        Err(other) => panic!("undo failed: {other}"),
    };
    assert_eq!(undo("no-such-repair"), "ARTIST_REPAIR_NOT_FOUND");

    // A credit changed since the repair: the undo is stale and changes
    // nothing.
    let rename = |name: &str| {
        db.execute(
            "UPDATE local_album_artists SET credited_name = ?2 WHERE local_album_id = ?1",
            rusqlite::params![album, name],
        )
        .expect("credit renamed");
    };
    rename("YES");
    let changed = read(album_credits);
    assert_eq!(
        history()[0]["undo_id"],
        Value::Null,
        "no Undo while it would not apply"
    );
    assert_eq!(undo(&repair), "STALE_REVISION");
    assert_eq!(
        read(album_credits),
        changed,
        "a refused undo changes nothing"
    );
    rename("Yes");

    assert_eq!(undo(&repair), "restored");
    assert_eq!(
        read(
            "SELECT album_artist_id || ' ' || album_artist_name || ' ' || \
             album_artist_name_folded || ' ' || album_artist_sort_name FROM local_albums"
        ),
        ["old-unknown Unknown Artist unknown artist Artist, Unknown"]
    );
    assert_eq!(
        read(album_credits),
        ["old-unknown Unknown Artist primary"],
        "the credits are back as they were"
    );
    assert_eq!(
        history()[0]["undo_id"],
        Value::Null,
        "the entry stays, without Undo"
    );
    assert_eq!(undo(&repair), "STALE_REVISION", "an undo runs once");

    // A cleanup of the album leaves it undone.
    let [cleanup] = read(
        "SELECT id FROM library_operation_jobs \
         WHERE idempotency_key = 'catalog-identity-hygiene:v1:backfill'",
    )
    .try_into()
    .expect("one cleanup job");
    let mut conn = droppedneedle_kernel::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens");
    let tx = conn.transaction().expect("transaction");
    let again = droppedneedle_library::repairs::hygiene::clean_album(&tx, &album, &cleanup, 0.0)
        .expect("cleanup runs");
    tx.commit().expect("commit");
    assert_eq!(again, None);
    assert_eq!(
        read("SELECT album_artist_name FROM local_albums"),
        ["Unknown Artist"],
        "an undone repair stays undone"
    );
}

/// The file's AcoustID values as its own tag frames hold them, read with
/// the test's byte readers: `kind` is "Id" or "Fingerprint".
fn raw_acoustid(path: &Path, kind: &str) -> Vec<String> {
    use crate::library_tags::{id3_txxx_values, mp4_freeform, vorbis_key_values};
    let name = format!("Acoustid {kind}");
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("flac") => vorbis_key_values(path, &format!("ACOUSTID_{}", kind.to_uppercase())),
        Some("mp3") => id3_txxx_values(path, &name),
        Some("m4a") => mp4_freeform(path, &name),
        other => panic!("no reader for {other:?}"),
    }
}

impl ManagedLibrary {
    /// Take both AcoustID tags off the tracks' files from outside the app.
    async fn strip_acoustid(&self, tracks: &[String]) {
        use droppedneedle_library::tags::{TagEdit, TagField, save_tags};
        for track in tracks {
            save_tags(
                &self.path(track),
                &[
                    TagEdit::verbatim(TagField::AcoustIdId, Vec::new()),
                    TagEdit::verbatim(TagField::AcoustIdFingerprint, Vec::new()),
                ],
            )
            .expect("AcoustID tags taken off");
        }
        rescan(&self.library).await;
    }
}

/// The Picard profile fills the AcoustID print from the file's own audio
/// in every writable format: FLAC, MP3 (ID3v2.4 and v2.3) and M4A. The
/// fixtures are shorter than a second, so no id is looked up.
#[tokio::test]
async fn acoustid_prints_are_written_from_the_audio() {
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::tags::generate_fingerprint;

    let managed = ManagedLibrary::start("lib-acoustid-print", |_| {}).await;
    let v23 = clean_v23_mp3(&managed.scratch.to_path_buf().join("v23.mp3"));
    let (album, tracks) = managed
        .add_album_from(
            "prints",
            &[
                "management_full.flac",
                "management_full.mp3",
                v23.as_str(),
                "management_full.m4a",
            ],
            |_| {},
        )
        .await;
    managed.strip_acoustid(&tracks).await;
    let expected: Vec<String> = tracks
        .iter()
        .map(|track| {
            let path = managed.path(track);
            assert!(raw_acoustid(&path, "Fingerprint").is_empty());
            generate_fingerprint(&path).expect("printed").fingerprint
        })
        .collect();

    let (handle, _) = managed
        .preview(SelectionKind::Albums, &[&album], "prints")
        .await;
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");

    for (track, print) in tracks.iter().zip(&expected) {
        let path = managed.path(track);
        assert_eq!(
            raw_acoustid(&path, "Fingerprint"),
            vec![print.clone()],
            "{}",
            path.display()
        );
        assert!(raw_acoustid(&path, "Id").is_empty(), "{}", path.display());
    }
}

/// A print kept as identification keeps one: for the file as it is on
/// disk now, with a real length (the fixtures are shorter than a second,
/// and a print of zero seconds is never looked up).
const SEEDED_PRINT: &str = "AQADtEqyPIuWIMwP_Qjl5HA949FhVUdwB4cO40c8";

impl ManagedLibrary {
    fn seed_print(&self, track: &str, revision: Option<&str>) {
        use droppedneedle_library::scan::revision::{exact_stat_revision, mtime_ns_from_metadata};
        let meta = std::fs::symlink_metadata(self.path(track)).expect("file stat");
        let on_disk = exact_stat_revision(meta.len(), mtime_ns_from_metadata(&meta));
        self.library
            .identify_store
            .save_fingerprint(track, revision.unwrap_or(&on_disk), SEEDED_PRINT, 218, true)
            .unwrap();
    }

    fn recording(&self, track: &str) -> String {
        self.library
            .identify_store
            .track_identity(track)
            .unwrap()
            .and_then(|mapping| mapping.recording_mbid)
            .expect("track mapped to a recording")
    }
}

/// An AcoustID result naming these recordings.
fn acoustid_result(
    id: &str,
    score: f64,
    recordings: &[&str],
) -> droppedneedle_providers::acoustid::BatchResult {
    use droppedneedle_providers::acoustid::{BatchRecording, BatchResult};
    BatchResult {
        id: id.to_owned(),
        score: Some(score),
        recordings: Some(
            recordings
                .iter()
                .map(|recording| BatchRecording {
                    id: (*recording).to_owned(),
                    releases: None,
                })
                .collect(),
        ),
    }
}

/// The value a planned file's AcoustID field gets, if it changes.
fn planned_acoustid(
    item: &droppedneedle_library::management::models::PlannedItem,
    field: &str,
) -> Option<Value> {
    item.diff
        .field_mutations
        .iter()
        .find(|mutation| mutation.name == field)
        .map(|mutation| mutation.after.clone())
}

const ID_A: &str = "a2558b28-8f9d-44ca-b97b-f800ff935a1b";
const ID_B: &str = "1d2a3b4c-0000-4000-8000-000000000002";

/// With replace, the AcoustID id written is the best-scoring result that
/// names the file's own recording; a result naming another recording
/// writes none; no key and an outage write none and say so, and keep
/// the id the file has. A kept print for another revision of the file
/// is not used.
#[tokio::test]
async fn acoustid_ids_come_from_a_lookup_naming_the_files_recording() {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    use droppedneedle_library::management::enrich::{ScriptedAcoustId, ScriptedEnrichment};
    use droppedneedle_library::management::models::SelectionKind;

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted(
        "lib-acoustid-id",
        |profile| {
            for field in &mut profile.metadata.fields {
                if field.field == "acoustid_id" {
                    field.mode = FieldMode::Replace;
                }
            }
        },
        scripted.clone(),
    )
    .await;
    let (album, tracks) = managed
        .add_album_from("ids", &["management_full.flac"], |_| {})
        .await;
    managed.strip_acoustid(&tracks).await;
    managed.seed_print(&tracks[0], None);
    let recording = managed.recording(&tracks[0]);
    let script = |answer: Option<ScriptedAcoustId>| {
        *scripted.acoustid.lock().expect("script") = answer;
    };
    let mut previews = 0;
    let mut plan = || {
        previews += 1;
        let key = format!("ids-{previews}");
        let managed = &managed;
        let album = album.clone();
        async move {
            let (handle, items) = managed
                .preview(SelectionKind::Albums, &[&album], &key)
                .await;
            (handle, items.into_iter().next().expect("one file"))
        }
    };
    let warned = |item: &droppedneedle_library::management::models::PlannedItem, start: &str| {
        item.facts
            .warnings
            .iter()
            .any(|warning| warning.starts_with(start))
    };

    script(Some(ScriptedAcoustId::Answers(vec![
        acoustid_result(ID_B, 0.6, &[&recording]),
        acoustid_result(ID_A, 0.9, &[&recording.to_uppercase()]),
    ])));
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), Some(json!(ID_A)));
    assert_eq!(
        planned_acoustid(&item, "acoustid_fingerprint"),
        Some(json!(SEEDED_PRINT))
    );
    let asked = scripted.acoustid_calls.lock().expect("calls").clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0][0].print, SEEDED_PRINT);
    assert_eq!(asked[0][0].duration_seconds, 218);

    script(Some(ScriptedAcoustId::Answers(vec![acoustid_result(
        ID_A,
        0.9,
        &["another-recording"],
    )])));
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), None);

    script(None);
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), None);
    assert!(
        warned(&item, "AcoustID IDs need an AcoustID key"),
        "{:?}",
        item.facts
    );

    script(Some(ScriptedAcoustId::Unavailable));
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), None);
    assert!(
        warned(&item, "AcoustID could not be reached"),
        "{:?}",
        item.facts
    );

    script(Some(ScriptedAcoustId::Answers(vec![acoustid_result(
        ID_A,
        0.9,
        &[&recording],
    )])));
    let (handle, _) = plan().await;
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    let file = managed.path(&tracks[0]);
    assert_eq!(raw_acoustid(&file, "Id"), vec![ID_A.to_owned()]);
    assert_eq!(
        raw_acoustid(&file, "Fingerprint"),
        vec![SEEDED_PRINT.to_owned()]
    );

    // Replace while AcoustID is down keeps the file's id.
    managed.seed_print(&tracks[0], None);
    script(Some(ScriptedAcoustId::Unavailable));
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), None);

    // A print kept for another revision of the file is not used: the
    // file's own audio is printed, and at under a second never looked up.
    managed.seed_print(&tracks[0], Some("1:1"));
    script(Some(ScriptedAcoustId::Answers(vec![acoustid_result(
        ID_B,
        0.9,
        &[&recording],
    )])));
    let calls = scripted.acoustid_calls.lock().expect("calls").len();
    let (_, item) = plan().await;
    assert_eq!(planned_acoustid(&item, "acoustid_id"), None);
    assert_eq!(scripted.acoustid_calls.lock().expect("calls").len(), calls);
}

/// While AcoustID is down an automatic look writes nothing and the album
/// waits, never parked; once AcoustID answers, the id lands by itself.
#[tokio::test]
async fn automatic_acoustid_waits_while_acoustid_is_down() {
    use droppedneedle_library::management::enrich::{ScriptedAcoustId, ScriptedEnrichment};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-acoustid-auto", |_| {}, scripted.clone()).await;
    *scripted.acoustid.lock().expect("script") = Some(ScriptedAcoustId::Unavailable);
    let (album, tracks) = managed
        .add_album_from("first", &["management_full.flac"], |_| {})
        .await;
    managed.strip_acoustid(&tracks).await;
    managed.seed_print(&tracks[0], None);
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    managed.activate().await;
    let file = managed.path(&tracks[0]);
    let before = file_sha256(&file);

    managed.look(&album).await;
    assert_eq!(
        file_sha256(&file),
        before,
        "nothing written while it is down"
    );
    let reason = |managed: &ManagedLibrary| -> (String, i64, f64) {
        managed
            .db()
            .query_row(
                "SELECT last_reason_code, attempts, not_before \
                 FROM library_management_automatic_queue WHERE local_album_id = ?1",
                [&album],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the album waits")
    };
    assert_eq!(reason(&managed).0, "ACOUSTID_UNAVAILABLE");
    managed
        .db()
        .execute(
            "UPDATE library_management_automatic_queue SET attempts = 7, not_before = 0 \
             WHERE local_album_id = ?1",
            [&album],
        )
        .expect("many tries");
    managed.worker_looks().await;
    let (code, attempts, not_before) = reason(&managed);
    assert_eq!((code.as_str(), attempts), ("ACOUSTID_UNAVAILABLE", 8));
    assert!(not_before < 1.0e12, "not parked: {not_before}");

    let recording = managed.recording(&tracks[0]);
    *scripted.acoustid.lock().expect("script") =
        Some(ScriptedAcoustId::Answers(vec![acoustid_result(
            ID_A,
            0.9,
            &[&recording],
        )]));
    droppedneedle_library::management::automatic::wake(
        &managed.db(),
        Some("ACOUSTID_UNAVAILABLE"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64(),
    )
    .expect("woken");
    managed.worker_looks().await;
    let file = managed.path(&tracks[0]);
    assert_eq!(raw_acoustid(&file, "Id"), vec![ID_A.to_owned()]);
}

/// Without a key (or with one AcoustID refuses) an automatic look does
/// not wait: the album is written with its print and no id.
#[tokio::test]
async fn automatic_acoustid_without_a_key_goes_ahead() {
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use rusqlite::OptionalExtension as _;

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-acoustid-nokey", |_| {}, scripted.clone()).await;
    let (album, tracks) = managed
        .add_album_from("nokey", &["management_full.flac"], |_| {})
        .await;
    managed.strip_acoustid(&tracks).await;
    managed.seed_print(&tracks[0], None);
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    managed.activate().await;

    managed.look(&album).await;
    let file = managed.path(&tracks[0]);
    assert_eq!(
        raw_acoustid(&file, "Fingerprint"),
        vec![SEEDED_PRINT.to_owned()]
    );
    assert!(raw_acoustid(&file, "Id").is_empty());
    let waiting: Option<String> = managed
        .db()
        .query_row(
            "SELECT last_reason_code FROM library_management_automatic_queue \
             WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .optional()
        .expect("queue read");
    assert_eq!(waiting, None);
}

/// Fill missing leaves a file that holds both AcoustID values alone, and
/// looks up only the file beside it that holds neither.
#[tokio::test]
async fn fill_missing_acoustid_looks_up_only_the_file_without_values() {
    use droppedneedle_library::management::enrich::{ScriptedAcoustId, ScriptedEnrichment};
    use droppedneedle_library::management::models::SelectionKind;

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-acoustid-fill", |_| {}, scripted.clone()).await;
    let (album, tracks) = managed
        .add_album_from(
            "fill",
            &["management_full.flac", "management_full.flac"],
            |_| {},
        )
        .await;
    managed.strip_acoustid(&tracks[1..]).await;
    let held = managed.path(&tracks[0]);
    assert!(!raw_acoustid(&held, "Id").is_empty());
    assert!(!raw_acoustid(&held, "Fingerprint").is_empty());
    managed.seed_print(&tracks[1], None);
    let recording = managed.recording(&tracks[1]);
    *scripted.acoustid.lock().expect("script") =
        Some(ScriptedAcoustId::Answers(vec![acoustid_result(
            ID_A,
            0.9,
            &[&recording],
        )]));

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "fill")
        .await;
    let item = |track: &str| {
        items
            .iter()
            .find(|item| item.track_id.as_deref() == Some(track))
            .expect("planned")
    };
    for field in ["acoustid_id", "acoustid_fingerprint"] {
        assert_eq!(planned_acoustid(item(&tracks[0]), field), None, "{field}");
    }
    assert_eq!(
        planned_acoustid(item(&tracks[1]), "acoustid_id"),
        Some(json!(ID_A))
    );
    let asked = scripted.acoustid_calls.lock().expect("calls").clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].len(),
        1,
        "only the file without values is looked up"
    );
}

// ---------------------------------------------------------------------------
// A file a scan adds to an album with an edition is placed by its own ids.
// ---------------------------------------------------------------------------

/// The edition a curator chose for the albums [`ManagedLibrary::chosen_with_ids`]
/// builds. Its tracks are `rt-N` with the recordings `rec-N`.
const CHOSEN_REL: &str = "33333333-3333-4333-8333-333333333333";

/// [`CHOSEN_REL`]'s document, with `count` tracks on one disc.
fn chosen_release(count: u32) -> Release {
    Release {
        id: CHOSEN_REL.to_owned(),
        release_group_id: "rg-chosen".to_owned(),
        title: "Chosen".to_owned(),
        artists: vec![CreditedArtist {
            id: "artist-1".to_owned(),
            name: "Chosen Artist".to_owned(),
            sort_name: None,
            join: String::new(),
        }],
        tracks: (1..=count)
            .map(|n| ReleaseTrack {
                id: format!("rt-{n}"),
                recording_id: format!("rec-{n}"),
                title: format!("Chosen {n}"),
                disc: 1,
                position: n,
                absolute_position: n,
                ..ReleaseTrack::default()
            })
            .collect(),
        ..Release::default()
    }
}

/// The edition's ids, as MusicBrainz tools write them, on track `n`.
fn chosen_ids(n: usize) -> Vec<droppedneedle_library::tags::TagEdit> {
    use droppedneedle_library::tags::{TagEdit, TagField};
    vec![
        TagEdit::verbatim(TagField::Title, vec![format!("Chosen {n}")]),
        TagEdit::verbatim(TagField::TrackNumber, vec![n.to_string()]),
        TagEdit::verbatim(TagField::DiscNumber, vec!["1".to_owned()]),
        TagEdit::verbatim(TagField::Album, vec!["Chosen".to_owned()]),
        TagEdit::verbatim(TagField::MusicBrainzReleaseId, vec![CHOSEN_REL.to_owned()]),
        // The fixture's release group id would name another album.
        TagEdit::verbatim(TagField::MusicBrainzReleaseGroupId, Vec::new()),
        TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, vec![format!("rt-{n}")]),
        TagEdit::verbatim(TagField::MusicBrainzRecordingId, vec![format!("rec-{n}")]),
    ]
}

/// A track's placement as a person's history reads it: release, release
/// track, source, disc and position.
type Placed = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

fn placed(db: &rusqlite::Connection, track: &str) -> Placed {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT release_mbid, release_track_mbid, decision_source, medium_position, \
         release_track_position FROM local_track_external_identities WHERE local_track_id = ?1",
        [track],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .optional()
    .expect("placement reads")
    .unwrap_or_default()
}

impl ManagedLibrary {
    /// `count` files carrying [`CHOSEN_REL`]'s ids planted in `folder`
    /// and scanned, on an album a curator chose that edition for long
    /// before, its tracks mapped and its document on file. The app has
    /// never written them. Returns the album and its tracks in order.
    async fn chosen_with_ids(&self, folder: &str, count: usize) -> (String, Vec<String>) {
        use droppedneedle_library::tags::save_tags;
        for n in 1..=count {
            let file = plant(
                &self.music,
                &format!("{folder}/{n:02}.flac"),
                "management_full.flac",
            );
            save_tags(&file, &chosen_ids(n)).expect("ids written");
        }
        rescan(&self.library).await;
        let tracks: Vec<String> = (1..=count)
            .map(|n| indexed_at(&self.library, &format!("{folder}/{n:02}.flac")))
            .collect();
        let album = self
            .library
            .scan_store
            .album_for_track(&tracks[0])
            .unwrap()
            .expect("album");
        let mut sql = format!(
            "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
             VALUES ('curator', 'Curator', 'admin', '2026-01-01T00:00:00Z'); \
             INSERT OR REPLACE INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision) VALUES ('{album}', 'musicbrainz', 'rg-chosen', \
             '{CHOSEN_REL}', 'manual', 'curator', 1234.5, 7);"
        );
        for (index, track) in tracks.iter().enumerate() {
            let n = index + 1;
            sql.push_str(&format!(
                "INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_mbid, release_track_mbid, medium_position, \
                 release_track_position, decision_source, selected_at) VALUES ('{track}', \
                 'musicbrainz', 'rec-{n}', '{CHOSEN_REL}', 'rt-{n}', 1, {n}, 'manual', 1);"
            ));
        }
        self.db().execute_batch(&sql).expect("edition chosen");
        self.library
            .identify_store
            .save_release(
                &chosen_release(count as u32),
                droppedneedle_kernel::db::Lane::Background,
            )
            .unwrap();
        (album, tracks)
    }

    /// Retitle every file of `tracks` to "Retagged", keeping their ids,
    /// and rename each into `retagged/`, in one go; scan. Returns the
    /// new paths.
    async fn retag_keeping_ids(&self, tracks: &[String]) -> Vec<PathBuf> {
        use droppedneedle_library::tags::{TagEdit, TagField, save_tags};
        let paths: Vec<PathBuf> = tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                let from = self.path(track);
                save_tags(
                    &from,
                    &[TagEdit::verbatim(
                        TagField::Album,
                        vec!["Retagged".to_owned()],
                    )],
                )
                .expect("retagged outside the app");
                let to = self.music.join(format!("retagged/{:02}.flac", index + 1));
                std::fs::create_dir_all(to.parent().expect("parent")).expect("mkdir");
                std::fs::rename(&from, &to).expect("renamed by hand");
                to
            })
            .collect();
        rescan(&self.library).await;
        paths
    }

    fn row_at(&self, path: &Path) -> String {
        let relative = path
            .strip_prefix(&self.music)
            .expect("under the root")
            .to_string_lossy()
            .into_owned();
        indexed_at(&self.library, &relative)
    }
}

/// An album whose edition a person chose, in a folder managed
/// automatically, that the app never wrote: every file retagged to a new
/// album name with its MusicBrainz ids kept, and renamed. At that scan's
/// end, with no identification, each new file is on its old file's track
/// as the person's, the album is left unmanaged, and the play, star and
/// playlist entry are on the new files. The worker writes nothing, and a
/// second scan changes nothing.
#[tokio::test]
async fn a_chosen_edition_retagged_with_its_ids_is_placed_and_left_to_the_person() {
    let managed = ManagedLibrary::start("lib-place-retag-kept-ids", |_| {}).await;
    let (album, old) = managed.chosen_with_ids("chosen", 2).await;
    managed.activate().await;
    let db = managed.db();
    db.execute_batch(&format!(
        "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
         track_name, artist_name, played_at) VALUES ('p1', 'admin', '{o1}', '{album}', \
         'One', 'A', '2026-01-01T00:00:00Z'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('admin', 'track', '{o1}', 1); \
         INSERT INTO playlists (id, name, created_at, updated_at) VALUES ('list', 'List', '', ''); \
         INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
         album_name, album_id, track_source_id, library_file_id, source_type, created_at) \
         VALUES ('entry', 'list', 0, 'Two', 'A', 'Chosen', '{album}', '{o2}', '{o2}', \
         'local', '');",
        o1 = old[0],
        o2 = old[1],
    ))
    .expect("people made things");
    let chosen = choice_row(&db, &album);

    let files = managed.retag_keeping_ids(&old).await;
    let new: Vec<String> = files.iter().map(|file| managed.row_at(file)).collect();
    for (index, track) in new.iter().enumerate() {
        assert!(!old.contains(track), "a new track row");
        assert_eq!(managed.album_of(track), album);
        let n = index as i64 + 1;
        assert_eq!(
            placed(&db, track),
            (
                Some(CHOSEN_REL.to_owned()),
                Some(format!("rt-{n}")),
                Some("manual".to_owned()),
                Some(1),
                Some(n)
            ),
            "placed at the scan's end"
        );
    }
    assert_eq!(choice_row(&db, &album), chosen, "the match is as it was");
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    assert_eq!(holdings(&db, &new[0])[..2], [1, 1], "play and star");
    assert_eq!(holdings(&db, &new[1])[2], 1, "playlist entry");
    assert_eq!(continuations(&db), 1);

    let bytes: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();
    managed.look(&album).await;
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    let after: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();
    assert_eq!(after, bytes, "nothing written");

    let actions = managed.count("SELECT COUNT(*) FROM library_catalog_actions");
    let revisions: Vec<i64> = new.iter().map(|track| placement(&db, track).2).collect();
    rescan(&managed.library).await;
    assert_eq!(
        managed.count("SELECT COUNT(*) FROM library_catalog_actions"),
        actions
    );
    let again: Vec<i64> = new.iter().map(|track| placement(&db, track).2).collect();
    assert_eq!(again, revisions, "placed once");
}

/// The same retag in a folder whose automatic management is off: the
/// album is marked at the scan's end, so a preview of it and another
/// album leaves it alone, and a preview of the album itself plans it with
/// the warning.
#[tokio::test]
async fn a_retag_placed_where_management_is_by_hand_is_left_out_of_wide_previews() {
    use droppedneedle_library::management::models::{Eligibility, SelectionKind};

    let managed = ManagedLibrary::start("lib-place-retag-by-hand", |_| {}).await;
    let (other, _) = managed.add_album("other", 1).await;
    let (album, old) = managed.chosen_with_ids("chosen", 2).await;
    managed.activate().await;
    managed.set_automatic(false, false);
    let files = managed.retag_keeping_ids(&old).await;
    assert_eq!(managed.mark(&album).as_deref(), Some("retag_followed"));
    let bytes: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album, &other], "both")
        .await;
    assert_eq!(items.len(), 3, "a file each");
    for item in &items {
        if item.album_id == album {
            assert_eq!(item.eligibility, Eligibility::Blocked);
            assert_eq!(item.reason().expect("a reason").code, "TAKEN_OVER");
        } else {
            assert_eq!(item.album_id, other);
            assert!(item.eligibility.writes(), "{:?}", item.facts);
        }
    }
    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "own")
        .await;
    assert_eq!(items.len(), 2);
    for item in &items {
        assert!(item.eligibility.writes(), "{:?}", item.facts);
        assert!(
            item.facts
                .warnings
                .iter()
                .any(|warning| warning.contains("Applying this preview overwrites those changes")),
            "{:?}",
            item.facts.warnings
        );
    }
    let after: Vec<String> = files.iter().map(|file| file_sha256(file)).collect();
    assert_eq!(after, bytes, "nothing written");
}

/// A person's edition the app never wrote, with a second copy of a track
/// beside the original, left over by an earlier placement with only its
/// recording: nothing moves while both are there. The original deleted,
/// the copy is placed on its track at that scan's end with the original's
/// star, the original's id opens it, and the album is not left unmanaged:
/// a finished scan saw both files. The worker then looks at it as usual.
#[tokio::test]
async fn a_copy_takes_the_place_of_a_deleted_original_on_a_chosen_edition() {
    let managed = ManagedLibrary::start("lib-place-copy-chosen", |_| {}).await;
    let (album, tracks) = managed.chosen_with_ids("chosen", 2).await;
    managed.activate().await;
    let db = managed.db();
    let original = &tracks[1];
    std::fs::copy(
        managed.path(original),
        managed.music.join("chosen/02 copy.flac"),
    )
    .expect("copied");
    rescan(&managed.library).await;
    let copy = indexed_at(&managed.library, "chosen/02 copy.flac");
    assert_eq!(managed.album_of(&copy), album);
    db.execute_batch(&format!(
        "INSERT INTO local_track_external_identities (local_track_id, provider, \
         recording_mbid, decision_source, selected_at) VALUES \
         ('{copy}', 'musicbrainz', 'rec-2', 'manual', 1); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('admin', 'track', '{original}', 1);"
    ))
    .expect("left over with its recording");
    rescan(&managed.library).await;
    assert_eq!(placed(&db, &copy).1, None, "both there: nothing placed");
    assert_eq!(holdings(&db, original)[1], 1, "nothing moves");
    assert_eq!(managed.mark(&album), None);

    std::fs::remove_file(managed.path(original)).expect("original deleted");
    rescan(&managed.library).await;
    assert_eq!(
        placed(&db, &copy),
        (
            Some(CHOSEN_REL.to_owned()),
            Some("rt-2".to_owned()),
            Some("manual".to_owned()),
            Some(1),
            Some(2)
        )
    );
    assert_eq!(holdings(&db, &copy)[1], 1);
    assert_eq!(holdings(&db, original)[1], 0);
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM local_track_aliases WHERE alias = '{original}' \
             AND local_track_id = '{copy}'"
        )),
        1,
        "the original's id opens the copy"
    );
    assert_eq!(managed.mark(&album), None, "a finished scan saw both");
    let runs = managed.automatic_runs().len();
    managed.look(&album).await;
    assert_eq!(managed.mark(&album), None);
    let after = managed.automatic_runs();
    assert!(after.len() > runs, "the worker plans the album");
    assert_eq!(after.last().map(|run| run.0.as_str()), Some("succeeded"));
}

/// An edition the app chose, with a second copy of a track beside the
/// original: when the original is deleted, the copy is placed on the
/// track as the app's own, revisable, and the original's star is on it.
#[tokio::test]
async fn a_copy_takes_the_place_of_a_deleted_original_on_an_automatic_edition() {
    use droppedneedle_library::tags::save_tags;

    let (scratch, library, music) = bare_library("lib-place-copy-automatic").await;
    for n in 1..=3 {
        let file = plant(&music, &format!("album/{n:02}.flac"), "flac_full_02.flac");
        save_tags(&file, &chosen_ids(n)).expect("ids written");
    }
    add_music_root(&library, &music).await;
    let old: Vec<String> = (1..=3)
        .map(|n| indexed_at(&library, &format!("album/{n:02}.flac")))
        .collect();
    let album = library
        .scan_store
        .album_for_track(&old[0])
        .unwrap()
        .expect("album");
    let db = scratch_db(&scratch);
    let mut recall = fitting_release(&library, &album);
    recall.releases[0].id = CHOSEN_REL.to_owned();
    scratch.identify.set_recall(recall);
    while library.identification.identify_tick().await > 0 {}
    let release_track = placement(&db, &old[1]).1.expect("original mapped");
    std::fs::copy(
        music.join("album/02.flac"),
        music.join("album/02 copy.flac"),
    )
    .expect("copied");
    rescan(&library).await;
    let copy = indexed_at(&library, "album/02 copy.flac");
    assert_eq!(
        library
            .scan_store
            .album_for_track(&copy)
            .unwrap()
            .as_deref(),
        Some(album.as_str())
    );
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'track', '{}', 1);",
        old[1]
    ))
    .expect("starred");
    rescan(&library).await;
    assert_eq!(placed(&db, &copy).1, None, "both there: nothing placed");

    std::fs::remove_file(music.join("album/02.flac")).expect("original deleted");
    rescan(&library).await;
    let (_, on, source, _, _) = placed(&db, &copy);
    assert_eq!(on, Some(release_track));
    assert_eq!(source.as_deref(), Some("automatic"));
    assert_eq!(holdings(&db, &copy)[1], 1);
    assert_eq!(holdings(&db, &old[1])[1], 0);
}

/// 坂本龍一, credited "Ryuichi Sakamoto", in the recorded names lookup.
const SAKAMOTO: &str = "a7f7df4a-77d8-4f12-8acd-5c60c93f4de8";
/// 高谷史郎, credited "Shiro Takatani", with no aliases.
const TAKATANI: &str = "7493b80f-de18-4f9c-a7bd-08d1ea5adcff";

/// The names MusicBrainz answers for _async surround_, through the
/// production parser.
fn recorded_names() -> droppedneedle_library::matching::model::ArtistNames {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/musicbrainz/release_names_sakamoto.json");
    let wire: droppedneedle_providers::musicbrainz::MbRelease =
        serde_json::from_slice(&std::fs::read(path).expect("probe reads")).expect("probe decodes");
    droppedneedle_library::identify::sources::names_from_wire(&wire, 0)
}

/// The edition credited as _async surround_ is: the album to Ryuichi
/// Sakamoto + Shiro Takatani, every track to Sakamoto.
fn sakamoto_edition(release: &mut Release) {
    let artist = |id: &str, name: &str, sort: &str, join: &str| CreditedArtist {
        id: id.to_owned(),
        name: name.to_owned(),
        sort_name: Some(sort.to_owned()),
        join: join.to_owned(),
    };
    release.artists = vec![
        artist(SAKAMOTO, "Ryuichi Sakamoto", "Sakamoto, Ryūichi", " + "),
        artist(TAKATANI, "Shiro Takatani", "Takatani, Shiro", ""),
    ];
    for track in &mut release.tracks {
        track.artists = vec![artist(
            SAKAMOTO,
            "Ryuichi Sakamoto",
            "Sakamoto, Ryūichi",
            "",
        )];
    }
}

/// The Picard profile translating artist names to English, with every
/// artist field on replace.
fn translating(
    profile: &mut droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
) {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    use droppedneedle_library::management::enrich::ARTIST_FIELDS;
    profile.metadata.artist_credits.translate_names = true;
    profile.metadata.artist_credits.preferred_locales = vec!["en".to_owned()];
    for field in &mut profile.metadata.fields {
        if ARTIST_FIELDS.contains(&field.field.as_str()) {
            field.mode = FieldMode::Replace;
        }
    }
}

/// An ID3 file's `TXXX` values under `description`, read from its bytes.
fn id3_user_values(path: &Path, description: &str) -> Vec<Vec<String>> {
    use crate::library_tags::{id3_frames, id3_strings};
    id3_frames(path)
        .1
        .into_iter()
        .filter(|(id, _)| id == "TXXX")
        .map(|(_, payload)| id3_strings(&payload))
        .filter(|strings| strings.first().map(String::as_str) == Some(description))
        .map(|strings| strings[1..].to_vec())
        .collect()
}

/// Translating to English writes each artist field in every format's
/// home: the primary English alias with its sort name, and Shiro
/// Takatani, who has no alias, from his sort name. The album folder
/// follows the translated album artist; Undo puts names and paths back; a
/// second plan asks MusicBrainz nothing; and after a rescan the album and
/// its artist are the same rows.
#[tokio::test]
async fn a_translating_profile_writes_english_artist_names() {
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::management::service::ApplyRequest;
    use droppedneedle_library::tags::{TagField, read_fields};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-names", translating, scripted.clone()).await;
    *scripted.names.lock().expect("script") = Some(recorded_names());
    let v23 = clean_v23_mp3(&managed.scratch.to_path_buf().join("v23.mp3"));
    let fixtures = [
        "management_full.flac",
        "management_full.mp3",
        v23.as_str(),
        "management_full.m4a",
    ];
    let (album, tracks) = managed
        .add_album_from("names", &fixtures, sakamoto_edition)
        .await;
    // The files credit the edition's artists, with their MusicBrainz ids,
    // under the names as credited.
    for track in &tracks {
        use droppedneedle_library::tags::{TagEdit, save_tags};
        let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        save_tags(
            &managed.path(track),
            &[
                TagEdit::verbatim(TagField::Artist, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::Artists, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::MusicBrainzArtistId, owned(&[SAKAMOTO])),
                TagEdit::verbatim(
                    TagField::AlbumArtist,
                    owned(&["Ryuichi Sakamoto + Shiro Takatani"]),
                ),
                TagEdit::verbatim(
                    TagField::AlbumArtists,
                    owned(&["Ryuichi Sakamoto", "Shiro Takatani"]),
                ),
                TagEdit::verbatim(
                    TagField::MusicBrainzAlbumArtistId,
                    owned(&[SAKAMOTO, TAKATANI]),
                ),
            ],
        )
        .expect("credited as the edition credits them");
    }
    rescan(&managed.library).await;
    let album_artist = |album: &str| -> String {
        managed
            .db()
            .query_row(
                "SELECT album_artist_id FROM local_albums WHERE id = ?1",
                [album],
                |row| row.get(0),
            )
            .expect("album row")
    };
    let artist_row = album_artist(&album);
    // The profile writes credits too: they come in the same lookup.
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    let before: Vec<(PathBuf, _)> = tracks
        .iter()
        .map(|track| {
            let path = managed.path(track);
            let tags = read_fields(&path).expect("tags read");
            (path, tags)
        })
        .collect();

    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&album], "names")
        .await;
    assert_eq!(scripted.lookups.lock().expect("lookups").len(), 1);
    assert!(items.iter().all(|item| item.eligibility.writes()));
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");

    let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    for track in &tracks {
        let file = managed.path(track);
        let name = file.display().to_string();
        let rel = file.strip_prefix(&managed.music).expect("in the library");
        assert!(
            rel.starts_with("Ryūichi Sakamoto + Shiro Takatani"),
            "the folder follows the album artist: {name}"
        );
        let fields = read_fields(&file).expect("tags read");
        let get = |field: TagField| fields.get(&field).cloned().unwrap_or_default();
        assert_eq!(
            get(TagField::Artist),
            owned(&["Ryūichi Sakamoto"]),
            "{name}"
        );
        assert_eq!(
            get(TagField::Artists),
            owned(&["Ryūichi Sakamoto"]),
            "{name}"
        );
        assert_eq!(
            get(TagField::ArtistSort),
            owned(&["Sakamoto, Ryūichi"]),
            "{name}"
        );
        assert_eq!(
            get(TagField::AlbumArtist),
            owned(&["Ryūichi Sakamoto + Shiro Takatani"]),
            "{name}"
        );
        assert_eq!(
            get(TagField::AlbumArtistSort),
            owned(&["Sakamoto, Ryūichi + Takatani, Shiro"]),
            "{name}"
        );
        if name.ends_with(".flac") {
            assert_eq!(
                crate::library_tags::vorbis_values(&file, "ALBUMARTISTS"),
                owned(&["Ryūichi Sakamoto", "Shiro Takatani"])
            );
        } else if file == managed.path(&tracks[2]) {
            // ID3v2.3 holds the list as one "/"-joined value.
            assert_eq!(
                id3_user_values(&file, "ALBUMARTISTS"),
                vec![owned(&["Ryūichi Sakamoto/Shiro Takatani"])]
            );
        } else if name.ends_with(".mp3") {
            assert_eq!(
                id3_user_values(&file, "ALBUMARTISTS"),
                vec![owned(&["Ryūichi Sakamoto", "Shiro Takatani"])]
            );
        } else {
            assert_eq!(
                crate::library_tags::mp4_freeform(&file, "ALBUMARTISTS"),
                owned(&["Ryūichi Sakamoto", "Shiro Takatani"]),
                "{name}"
            );
        }
    }

    // The names are on the stored edition: a second plan asks nothing.
    let (_, again) = managed
        .preview(SelectionKind::Albums, &[&album], "names-again")
        .await;
    assert_eq!(scripted.lookups.lock().expect("lookups").len(), 1);
    assert!(
        again
            .iter()
            .all(|item| item.diff.field_mutations.is_empty()),
        "nothing left to change"
    );

    // A rescan keeps the album and its artist.
    rescan(&managed.library).await;
    assert_eq!(
        managed
            .library
            .scan_store
            .album_for_track(&tracks[0])
            .unwrap()
            .as_deref(),
        Some(album.as_str())
    );
    assert_eq!(album_artist(&album), artist_row);

    // Undo puts the names and the paths back.
    let management = managed.management();
    let source = management.detail(&handle.job_id).expect("detail");
    let undo = management
        .undo_preview(
            &handle.job_id,
            source.job.row_revision,
            "names-undo",
            "admin",
        )
        .expect("undo preview queued");
    assert!(
        management_step(&managed.library).await,
        "undo planning runs"
    );
    let undo_detail = management.detail(&undo.job_id).expect("undo detail");
    assert!(
        undo_detail.ready_for_confirmation,
        "{:?}",
        undo_detail.stale_reasons
    );
    management
        .apply(
            &undo.job_id,
            ApplyRequest {
                preview_token: undo.preview_token.clone(),
                expected_row_revision: undo_detail.job.row_revision,
                idempotency_key: "names-undo-apply".to_owned(),
                confirmation: true,
            },
        )
        .expect("undo queued");
    assert!(management_step(&managed.library).await, "undo runs");
    // ID3v2.3 puts a list back as one "/"-joined value.
    let joined = |tags: &std::collections::BTreeMap<TagField, Vec<String>>| {
        tags.iter()
            .map(|(field, values)| (*field, values.join("/")))
            .collect::<Vec<_>>()
    };
    for (path, tags) in &before {
        let restored = read_fields(path).expect("restored file reads");
        assert_eq!(
            joined(&restored),
            joined(tags),
            "{} is back",
            path.display()
        );
    }
}

/// The edition crediting 坂本龍一 alone, as MusicBrainz names him, on the
/// album and every track.
fn sakamoto_alone(release: &mut Release) {
    let sakamoto = CreditedArtist {
        id: SAKAMOTO.to_owned(),
        name: "坂本龍一".to_owned(),
        sort_name: Some("Sakamoto, Ryūichi".to_owned()),
        join: String::new(),
    };
    release.artists = vec![sakamoto.clone()];
    for track in &mut release.tracks {
        track.artists = vec![sakamoto.clone()];
    }
}

/// Rows of a query's first column, as text.
fn texts(db: &rusqlite::Connection, sql: &str) -> Vec<String> {
    let mut statement = db.prepare(sql).expect("query prepares");
    statement
        .query_map([], |row| row.get(0))
        .expect("query runs")
        .collect::<Result<_, _>>()
        .expect("rows read")
}

/// A download whose files carry no MusicBrainz artist ids, credited
/// "Ryuichi Sakamoto", matched to an edition crediting 坂本龍一. Planning
/// the translating run links the artist to MusicBrainz through his names;
/// the run writes his id and English name in one go, and after a rescan
/// the artist is the same record, with its favorite and play.
#[tokio::test]
async fn a_run_that_adds_ids_and_translates_keeps_the_artist() {
    use droppedneedle_catalog::reads::library::store::LocalCatalog;
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::tags::{TagEdit, TagField, save_tags};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-names-link", translating, scripted.clone()).await;
    *scripted.names.lock().expect("script") = Some(recorded_names());
    let fixtures = [
        "management_full.flac",
        "management_full.mp3",
        "management_full.m4a",
    ];
    let (album, tracks) = managed
        .add_album_from("download", &fixtures, sakamoto_alone)
        .await;
    let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    for track in &tracks {
        save_tags(
            &managed.path(track),
            &[
                TagEdit::verbatim(TagField::Artist, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::Artists, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::ArtistSort, Vec::new()),
                TagEdit::verbatim(TagField::AlbumArtist, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::AlbumArtists, owned(&["Ryuichi Sakamoto"])),
                TagEdit::verbatim(TagField::AlbumArtistSort, Vec::new()),
                TagEdit::verbatim(TagField::MusicBrainzArtistId, Vec::new()),
                TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, Vec::new()),
            ],
        )
        .expect("tagged without artist ids");
    }
    rescan(&managed.library).await;
    let db = managed.db();
    let rows_of_album = format!(
        "SELECT c.local_artist_id FROM local_track_artists c JOIN local_tracks t \
         ON t.id = c.local_track_id WHERE t.local_album_id = '{album}' \
         UNION ALL SELECT local_artist_id FROM local_album_artists WHERE local_album_id = '{album}' \
         UNION ALL SELECT album_artist_id FROM local_albums WHERE id = '{album}'"
    );
    let artist = texts(&db, &rows_of_album)[0].clone();
    assert_eq!(
        texts(&db, &rows_of_album),
        vec![artist.clone(); tracks.len() + 2]
    );
    let link = format!(
        "SELECT provider_artist_id || ' ' || decision_source \
         FROM local_artist_external_identities WHERE local_artist_id = '{artist}'"
    );
    assert_eq!(texts(&db, &link), Vec::<String>::new(), "no link yet");
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('admin', 'artist', '{artist}', 1); \
         INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
         artist_name, played_at) VALUES ('play-1', 'admin', '{artist}', 'Song', \
         'Ryuichi Sakamoto', '2026-01-01T00:00:00Z');"
    ))
    .expect("favorite and play seeded");
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));

    let (handle, items) = managed
        .preview(SelectionKind::Albums, &[&album], "download")
        .await;
    assert!(
        items.iter().all(|item| item.eligibility.writes()),
        "{:?}",
        items.iter().map(|item| &item.facts).collect::<Vec<_>>()
    );
    assert_eq!(
        texts(&db, &link),
        [format!("{SAKAMOTO} automatic")],
        "planning linked the artist"
    );
    let applied = management_apply(&managed.library, &handle).await;
    assert_eq!(applied.job.state.as_str(), "succeeded");
    let results = managed
        .management()
        .results(&handle.job_id, -1, 50)
        .expect("results");
    for item in &results.items {
        assert_eq!(
            item.work_state,
            ResultState::Succeeded,
            "{:?}",
            item.failure
        );
        assert!(
            item.failure
                .as_ref()
                .is_none_or(|failure| failure.code != "ALBUM_CHANGED"),
            "{:?}",
            item.failure
        );
    }

    rescan(&managed.library).await;
    assert_eq!(
        texts(&db, &rows_of_album),
        vec![artist.clone(); tracks.len() + 2]
    );
    assert_eq!(
        texts(
            &db,
            &format!("SELECT display_name FROM local_artists WHERE id = '{artist}'")
        ),
        ["Ryūichi Sakamoto"]
    );
    assert_eq!(
        texts(
            &db,
            "SELECT item_id FROM library_user_favorites WHERE item_kind = 'artist' \
             UNION ALL SELECT local_artist_id FROM library_play_history"
        ),
        vec![artist.clone(); 2]
    );
    let reader =
        droppedneedle_kernel::db::Db::open_for_tests(&managed.scratch.to_path_buf().join("app.db"))
            .expect("reader opens");
    let read = LocalCatalog::new(reader)
        .get_artist(&artist)
        .await
        .expect("artist reads")
        .expect("artist found");
    assert_eq!(read.artist_mbid.as_deref(), Some(SAKAMOTO));
    assert_eq!(
        texts(
            &db,
            &format!(
                "SELECT id FROM local_artists WHERE display_name = 'Ryūichi Sakamoto' \
                 AND retired_into_artist_id IS NULL AND id <> '{artist}'"
            )
        ),
        Vec::<String>::new()
    );
    let revision = format!("SELECT row_revision FROM local_artists WHERE id = '{artist}'");
    let before: i64 = db
        .query_row(&revision, [], |row| row.get(0))
        .expect("revision");
    rescan(&managed.library).await;
    let after: i64 = db
        .query_row(&revision, [], |row| row.get(0))
        .expect("revision");
    assert_eq!(after, before, "a rescan with nothing new");
}

/// One identify tick that matches an album links its artist to the
/// MusicBrainz artist the release credits, though its files carry no
/// artist id.
#[tokio::test]
async fn an_identified_album_links_its_artist() {
    use droppedneedle_library::tags::{TagEdit, TagField, save_tags};

    let (scratch, library, music) = bare_library("lib-identify-links").await;
    let file = plant(&music, "album/01.flac", "management_full.flac");
    save_tags(
        &file,
        &[
            TagEdit::verbatim(TagField::MusicBrainzArtistId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, Vec::new()),
        ],
    )
    .expect("no artist ids");
    add_music_root(&library, &music).await;
    let track = indexed_at(&library, "album/01.flac");
    let album = library
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let release = tagged_release(&library.identify_store, &album);
    scratch.identify.set_recall(RecallResult {
        releases: vec![release.clone()],
        ..RecallResult::default()
    });
    assert_eq!(library.identification.identify_tick().await, 1);
    let db = scratch_db(&scratch);
    assert_eq!(
        texts(
            &db,
            &format!(
                "SELECT i.provider_artist_id || ' ' || i.decision_source \
                 FROM local_albums a JOIN local_artist_external_identities i \
                 ON i.local_artist_id = a.album_artist_id WHERE a.id = '{album}'"
            )
        ),
        [format!("{} automatic", release.artists[0].id)]
    );
}

/// An artist whose older files carry an id MusicBrainz has since merged
/// away. A newer album tagged with the current id lands on a second row.
/// Once the older album is matched to an edition crediting the current id,
/// MusicBrainz's answer about the old id joins the two rows into the
/// older one, which keeps its favorite and play and is linked to the
/// current id. A full rescan and a run that writes the current id into
/// the files keep it that way.
#[tokio::test]
async fn an_artist_whose_files_carry_a_merged_id_is_linked_and_kept() {
    use droppedneedle_library::identify::sources::ArtistAnswer;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    use droppedneedle_library::tags::{TagField, read_fields};

    // Recorded from MusicBrainz on 2026-10-17: the first id answers with a
    // redirect to the second.
    const RETIRED: &str = "3ec87112-9081-467b-8327-e18286f1b020";
    const CURRENT: &str = "ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea";
    let rig = Rig::start("lib-merged-artist-id").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let paused = rig.pause_identification().await;
    let music = app.config.root_app_dir.join("music");
    let one = |value: &str| vec![value.to_owned()];
    let tag = |file: &Path, number: usize, album: &str, id: &str, recording: &str| {
        let mut edits = vec![
            TagEdit::verbatim(TagField::Title, one(&format!("{album} {number}"))),
            TagEdit::verbatim(TagField::TrackNumber, one(&number.to_string())),
            TagEdit::verbatim(TagField::Album, one(album)),
            TagEdit::verbatim(TagField::Compilation, Vec::new()),
            TagEdit::verbatim(TagField::Artist, one("Artist X")),
            TagEdit::verbatim(TagField::Artists, one("Artist X")),
            TagEdit::verbatim(TagField::ArtistSort, Vec::new()),
            TagEdit::verbatim(TagField::AlbumArtist, one("Artist X")),
            TagEdit::verbatim(TagField::AlbumArtists, one("Artist X")),
            TagEdit::verbatim(TagField::AlbumArtistSort, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzArtistId, one(id)),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, one(id)),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, one(recording)),
        ];
        if album == "Later" {
            edits.push(TagEdit::verbatim(
                TagField::MusicBrainzReleaseId,
                Vec::new(),
            ));
            edits.push(TagEdit::verbatim(
                TagField::MusicBrainzReleaseGroupId,
                Vec::new(),
            ));
        }
        save_tags(file, &edits).expect("fixture tagged");
    };
    let first = [
        ("first/01.flac", "management_full.flac"),
        ("first/02.mp3", "management_full.mp3"),
        ("first/03.m4a", "management_full.m4a"),
    ];
    for (index, (relative, fixture)) in first.iter().enumerate() {
        let recording = format!("30000000-0000-4000-8000-00000000010{}", index + 1);
        tag(
            &plant(&music, relative, fixture),
            index + 1,
            "First",
            RETIRED,
            &recording,
        );
    }
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "first/01.flac")
        .unwrap()
        .expect("track indexed");
    let album = rig
        .scan_store
        .album_for_track(&track)
        .unwrap()
        .expect("album");
    let db = app.db();
    let text = |sql: &str| texts(&db, sql);
    let artist = text(&format!(
        "SELECT album_artist_id FROM local_albums WHERE id = '{album}'"
    ))[0]
        .clone();
    let user = text("SELECT id FROM auth_users WHERE role = 'admin'")[0].clone();
    db.execute_batch(&format!(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('{user}', 'artist', '{artist}', 1); \
         INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
         artist_name, played_at) VALUES ('play-1', '{user}', '{artist}', 'First 1', \
         'Artist X', '2026-01-01T00:00:00Z');"
    ))
    .expect("favorite and play seeded");

    tag(
        &plant(&music, "later/01.flac", "management_full.flac"),
        1,
        "Later",
        CURRENT,
        "30000000-0000-4000-8000-000000000901",
    );
    app.scan_library().await;
    let later = rig
        .scan_store
        .track_at("music", "later/01.flac")
        .unwrap()
        .expect("later indexed");
    let later_rows = format!(
        "SELECT local_artist_id FROM local_track_artists WHERE local_track_id = '{later}' \
         UNION SELECT a.album_artist_id FROM local_albums a JOIN local_tracks t \
         ON t.local_album_id = a.id WHERE t.id = '{later}'"
    );
    let split = text(&later_rows);
    assert_eq!(split.len(), 1, "{split:?}");
    assert_ne!(split[0], artist, "the current id lands on a row of its own");
    let newer = split[0].clone();

    rig.identify.recall_from(|facts| {
        if facts.title != "First" {
            return RecallResult::default();
        }
        let mut release = tagged_from(facts);
        let credit = CreditedArtist {
            id: CURRENT.to_owned(),
            name: "Artist X".to_owned(),
            sort_name: None,
            join: String::new(),
        };
        release.artists = vec![credit.clone()];
        for track in &mut release.tracks {
            track.artists = vec![credit.clone()];
        }
        RecallResult {
            releases: vec![release],
            ..RecallResult::default()
        }
    });
    rig.identify
        .set_artist(RETIRED, ArtistAnswer::MergedInto(CURRENT.to_owned()));
    rig.identify.set_artist(CURRENT, ArtistAnswer::Current);
    rig.resume_identification(paused).await;
    let link = format!(
        "SELECT provider_artist_id || ' ' || decision_source \
         FROM local_artist_external_identities WHERE local_artist_id = '{artist}'"
    );
    let checks = "SELECT artist_mbid || ' ' || state || ' ' || COALESCE(canonical_mbid, '') \
                  FROM library_identify_artist_id_checks ORDER BY artist_mbid";
    app.until("the merged id is followed", || {
        let done = texts(&db, &link) == [format!("{CURRENT} automatic")]
            && texts(&db, checks).len() == 2
            && !texts(&db, checks)
                .iter()
                .any(|row| row.contains(" pending "));
        async move { done.then_some(()) }
    })
    .await;

    assert_eq!(
        text(checks),
        [
            format!("{RETIRED} merged {CURRENT}"),
            format!("{CURRENT} kept "),
        ]
    );
    let held_ids = format!(
        "SELECT artist_mbid FROM local_track_artists WHERE local_artist_id = '{artist}' \
         UNION SELECT artist_mbid FROM local_album_artists WHERE local_artist_id = '{artist}'"
    );
    assert_eq!(text(&held_ids), [CURRENT]);
    assert_eq!(
        text(&format!(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = '{newer}'"
        )),
        [artist.as_str()]
    );
    assert_eq!(
        text(&format!(
            "SELECT kind FROM local_artist_aliases WHERE alias = '{newer}'"
        )),
        ["merged_artist"]
    );
    assert_eq!(text(&later_rows), [artist.as_str()]);
    assert_eq!(
        text(
            "SELECT CAST(count(*) AS TEXT) FROM library_catalog_actions \
             WHERE reason_code = 'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'"
        ),
        ["1"]
    );
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/artists/{artist}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["artist_mbid"], json!(CURRENT), "{body}");
    let people_made = "SELECT item_id FROM library_user_favorites WHERE item_kind = 'artist' \
                       UNION ALL SELECT local_artist_id FROM library_play_history";
    assert_eq!(text(people_made), vec![artist.clone(); 2]);
    let asked = rig.identify.artists_asked();
    assert_eq!(
        asked.iter().filter(|id| id.as_str() == RETIRED).count(),
        1,
        "{asked:?}"
    );
    assert_eq!(
        asked.iter().filter(|id| id.as_str() == CURRENT).count(),
        1,
        "{asked:?}"
    );

    // A full rescan reads the old id in First's files again.
    let rows = "SELECT DISTINCT c.local_artist_id || ' ' || c.artist_mbid \
                FROM local_track_artists c JOIN local_tracks t ON t.id = c.local_track_id \
                WHERE t.availability = 'indexed' UNION SELECT DISTINCT local_artist_id || ' ' \
                || artist_mbid FROM local_album_artists \
                WHERE local_album_id IN (SELECT local_album_id FROM local_tracks)";
    let revision =
        format!("SELECT CAST(row_revision AS TEXT) FROM local_artists WHERE id = '{artist}'");
    let before = text(&revision);
    let (status, body) = app
        .call("GET", "/api/v3/settings/library", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let policy = body["policy_revision"].as_str().expect("policy").to_owned();
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/scan/runs",
            &headers,
            Some(json!({
                "kind": "rescan_files",
                "scope_ids": ["music"],
                "expected_policy_revision": policy,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    app.scan_ended(body["run_id"].as_str().expect("run id"))
        .await;
    assert_eq!(text(rows), [format!("{artist} {CURRENT}")]);
    assert_eq!(text(&revision), before, "the rescan changes nothing");

    // A run that writes MusicBrainz's ids into First's files, once any
    // identification the rescan queued for it has ended.
    rig.identification_idle(&album).await;
    let retag =
        |rel_path: &str| json!({"root_id": "music", "rel_path": rel_path, "managed_updates": {}});
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/preview",
            &headers,
            Some(json!({
                "kind": "retag",
                "album_id": album,
                "items": first.iter().map(|(relative, _)| retag(relative)).collect::<Vec<_>>(),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/library/manage/apply",
            &headers,
            Some(json!({"preview_token": body["preview_token"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains("ALBUM_CHANGED"), "{body}");
    let written = read_fields(&music.join("first/01.flac")).expect("fields read");
    assert_eq!(
        written[&TagField::MusicBrainzArtistId],
        vec![CURRENT.to_owned()]
    );
    app.scan_library().await;
    assert_eq!(text(rows), [format!("{artist} {CURRENT}")]);
    assert_eq!(
        text(&format!(
            "SELECT id FROM local_artists WHERE display_name = 'Artist X' \
             AND retired_into_artist_id IS NULL AND id <> '{artist}'"
        )),
        Vec::<String>::new()
    );
    rig.stop().await;
}

/// A custom edition under a release group credited to another artist is
/// refused while a person linked the album's artist, and sealed while the
/// link is the app's own, which a person has no other way to correct.
#[tokio::test]
async fn a_custom_edition_passes_over_the_apps_artist_link() {
    const GROUP: &str = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
    const OTHER_GROUP: &str = "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f";
    const OTHER: &str = "0f6e7a1c-2b3d-4e5f-8a9b-1c2d3e4f5a6b";
    const LINKED: &str = "aaaaaaaa-0000-4000-8000-0000000000aa";

    let managed = ManagedLibrary::start("lib-custom-link", |_| {}).await;
    let (album, _) = managed
        .add_album_from("custom", &["management_full.flac"], |release| {
            release.release_group_id = if release.id == REL_1 {
                GROUP.to_owned()
            } else {
                OTHER_GROUP.to_owned()
            };
        })
        .await;
    let db = managed.db();
    let (release_id, revision, artist): (String, i64, String) = db
        .query_row(
            "SELECT i.release_mbid, i.row_revision, a.album_artist_id \
             FROM local_album_external_identities i JOIN local_albums a \
             ON a.id = i.local_album_id WHERE i.local_album_id = ?1",
            [&album],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("matched album");
    let mut release = managed
        .library
        .identify_store
        .release(&release_id, None)
        .unwrap()
        .expect("release on file");
    for credit in &mut release.artists {
        credit.id = OTHER.to_owned();
    }
    managed.scratch.identify.set_recall(RecallResult {
        releases: vec![release],
        ..RecallResult::default()
    });
    let link = |source: &str| {
        db.execute_batch(&format!(
            "DELETE FROM local_artist_external_identities WHERE local_artist_id = '{artist}'; \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES ('{artist}', 'musicbrainz', '{LINKED}', '{source}', 1);"
        ))
        .expect("link set");
    };
    let operations = crate::common::library::operations(&managed.library);

    link("manual");
    let refused = operations
        .seal_custom_edition(&album, &release_id, revision, "admin")
        .await
        .expect_err("a person's link stands");
    let conflict = droppedneedle_library::operations::reasons::CUSTOM_EDITION_ARTIST_CONFLICT;
    assert!(
        format!("{refused:?}").contains(conflict.message),
        "{refused:?}"
    );

    link("automatic");
    operations
        .seal_custom_edition(&album, &release_id, revision, "admin")
        .await
        .expect("sealed over the app's link");
    assert!(active_manifest(&db, &album).is_some());
}

/// While MusicBrainz is down a preview keeps the six artist tags, says
/// why, and writes the rest; an automatic look waits; once MusicBrainz
/// answers, waking the queue writes the translated names by itself.
#[tokio::test]
async fn translated_names_wait_for_musicbrainz() {
    use droppedneedle_library::management::enrich::ScriptedEnrichment;
    use droppedneedle_library::management::models::SelectionKind;
    use droppedneedle_library::tags::{TagField, read_fields};

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-names-down", translating, scripted.clone()).await;
    let (album, tracks) = managed
        .add_album_from("down", &["management_full.flac"], sakamoto_edition)
        .await;
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    let file = managed.path(&tracks[0]);
    let original = read_fields(&file).expect("tags read");

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "names-down")
        .await;
    let item = &items[0];
    assert!(item.eligibility.writes(), "{:?}", item.facts);
    assert!(
        item.facts.warnings.contains(
            &"MusicBrainz artist names could not be read, so the file keeps its artist tags."
                .to_owned()
        ),
        "{:?}",
        item.facts.warnings
    );
    let changed: Vec<&str> = item
        .diff
        .field_mutations
        .iter()
        .map(|mutation| mutation.name.as_str())
        .collect();
    assert!(!changed.is_empty(), "the other fields are written");
    for field in droppedneedle_library::management::enrich::ARTIST_FIELDS {
        assert!(!changed.contains(&field), "{field} kept: {changed:?}");
    }

    managed.activate().await;
    let before = file_sha256(&file);
    managed.look(&album).await;
    assert_eq!(
        file_sha256(&file),
        before,
        "nothing written while it is down"
    );
    let waiting: String = managed
        .db()
        .query_row(
            "SELECT last_reason_code FROM library_management_automatic_queue \
             WHERE local_album_id = ?1",
            [&album],
            |row| row.get(0),
        )
        .expect("the album waits");
    assert_eq!(waiting, "METADATA_UNAVAILABLE");

    *scripted.names.lock().expect("script") = Some(recorded_names());
    droppedneedle_library::management::automatic::wake(
        &managed.db(),
        Some("METADATA_UNAVAILABLE"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64(),
    )
    .expect("woken");
    managed.worker_looks().await;
    let fields = read_fields(&managed.path(&tracks[0])).expect("tags read");
    assert_eq!(
        fields[&TagField::Artist],
        vec!["Ryūichi Sakamoto".to_owned()]
    );
    assert_ne!(
        fields.get(&TagField::Artist),
        original.get(&TagField::Artist)
    );
}

/// A profile writing credits and translated names asks MusicBrainz once
/// per album, for both parts together.
#[tokio::test]
async fn credits_and_names_come_in_one_lookup() {
    use droppedneedle_library::management::enrich::{ExtrasWanted, ScriptedEnrichment};
    use droppedneedle_library::management::models::SelectionKind;

    let scripted = Arc::new(ScriptedEnrichment::default());
    let managed = ManagedLibrary::scripted("lib-names-once", translating, scripted.clone()).await;
    let (album, tracks) = managed
        .add_album_from(
            "once",
            &["management_full.flac", "management_full.flac"],
            sakamoto_edition,
        )
        .await;
    *scripted.relations.lock().expect("script") =
        Some(managed.scripted_credits(&tracks, |_| concerto_credits()));
    *scripted.names.lock().expect("script") = Some(recorded_names());

    let (_, items) = managed
        .preview(SelectionKind::Albums, &[&album], "once")
        .await;
    assert!(items.iter().all(|item| item.eligibility.writes()));
    let lookups = scripted.lookups.lock().expect("lookups").clone();
    assert_eq!(
        lookups,
        vec![(
            ExtrasWanted {
                relations: true,
                names: true
            },
            false
        )]
    );
}

impl ManagedLibrary {
    /// Retag every one of `tracks` outside the app to the album "Retagged"
    /// with its MusicBrainz ids removed, and rename it `NN renamed.flac`:
    /// in its own folder, or in the folder `sibling` next to it. Scans, and
    /// returns the new rows in order.
    async fn retag_by_hand(&self, tracks: &[String], sibling: Option<&str>) -> Vec<String> {
        use droppedneedle_library::tags::{TagEdit, TagField, save_tags};
        let mut moved = Vec::new();
        for (index, track) in tracks.iter().enumerate() {
            let from = self.path(track);
            let mut edits = vec![TagEdit::verbatim(
                TagField::Album,
                vec!["Retagged".to_owned()],
            )];
            edits.extend(
                [
                    TagField::MusicBrainzReleaseId,
                    TagField::MusicBrainzReleaseGroupId,
                    TagField::MusicBrainzReleaseTrackId,
                    TagField::MusicBrainzRecordingId,
                ]
                .map(|field| TagEdit::verbatim(field, Vec::new())),
            );
            save_tags(&from, &edits).expect("retagged outside the app");
            let folder = from.parent().expect("album folder");
            let folder = match sibling {
                Some(name) => folder.parent().expect("parent folder").join(name),
                None => folder.to_path_buf(),
            };
            let dest = folder.join(format!("{:02} renamed.flac", index + 1));
            std::fs::create_dir_all(&folder).expect("mkdir");
            std::fs::rename(&from, &dest).expect("renamed outside the app");
            moved.push(
                dest.strip_prefix(&self.music)
                    .expect("under the root")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        rescan(&self.library).await;
        moved
            .iter()
            .map(|path| indexed_at(&self.library, path))
            .collect()
    }

    fn edition_revision(&self, album: &str) -> i64 {
        use rusqlite::OptionalExtension as _;
        self.db()
            .query_row(
                "SELECT row_revision FROM local_album_external_identities \
                 WHERE local_album_id = ?1",
                [album],
                |row| row.get(0),
            )
            .optional()
            .expect("edition reads")
            .unwrap_or(0)
    }

    /// Identification of `album` run until it is idle, MusicBrainz
    /// offering two editions its files fit, and the match confirmed.
    async fn identify_and_confirm(&self, album: &str) {
        let mut recall = ambiguous_recall(&self.library.identify_store, album);
        for (edition, release) in recall.releases.iter_mut().enumerate() {
            release.id = format!("000000ff-{edition:04x}-4111-8111-111111111111");
            release.release_group_id = format!("rg-ff-{edition}");
        }
        self.scratch.identify.set_recall(recall);
        while self.library.identification.identify_tick().await > 0 {}
        crate::common::library::operations(&self.library)
            .confirm_match(album, "admin")
            .expect("match confirmed");
    }

    fn bytes(&self, tracks: &[String]) -> Vec<String> {
        tracks
            .iter()
            .map(|track| file_sha256(&self.path(track)))
            .collect()
    }
}

/// An album the app wrote, then left unmanaged by a person, whose files
/// are retagged outside the app to a new name with their MusicBrainz ids
/// removed and renamed in their folder: the new album is identified like
/// any other, and automatic management leaves its files alone. The two
/// are offered as a pair; the old album keeps its id, "leave unmanaged",
/// star and rows. "Manage it again" writes the new album, and a later scan
/// does not hold it again.
#[tokio::test]
async fn files_retagged_in_place_of_an_unmanaged_album_are_left_alone() {
    use droppedneedle_library::corrections::models::PairReason;
    use droppedneedle_library::mutations::Mutations;

    let managed = ManagedLibrary::start("lib-hands-off-replaced", |_| {}).await;
    let (album, old) = managed.add_album("first", 2).await;
    managed.add_album("filler", 1).await;
    managed.activate().await;
    managed.look(&album).await;
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert!(
        managed.recorded(&old[0]).is_some(),
        "the app wrote the files"
    );
    crate::common::library::operations(&managed.library)
        .leave_unmanaged(&album, managed.edition_revision(&album), "admin")
        .expect("left unmanaged");
    let db = managed.db();
    db.execute(
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('listener', 'album', ?1, 1)",
        [&album],
    )
    .expect("album starred");

    let new = managed.retag_by_hand(&old, None).await;
    let replacement = managed.album_of(&new[0]);
    assert_ne!(replacement, album, "an album row of its own");
    let bytes = managed.bytes(&new);
    managed.identify_and_confirm(&replacement).await;
    // A plan is made on one look and applied on the next.
    managed.look(&replacement).await;
    managed.worker_looks().await;

    assert_eq!(managed.bytes(&new), bytes, "nothing written");
    assert_eq!(
        managed.mark(&replacement).as_deref(),
        Some("replaced_hands_off")
    );
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
             WHERE local_album_id = '{replacement}' AND last_reason_code = 'TAKEN_OVER'"
        )),
        1,
        "parked"
    );
    let corrections = crate::common::library::services(&managed.library).corrections;
    let listed = corrections.retag_pairs().expect("pairs listed");
    let pairs: Vec<_> = listed
        .pairs
        .iter()
        .map(|pair| {
            (
                pair.old_album.id.clone(),
                pair.new_album.id.clone(),
                pair.reason,
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![(
            album.clone(),
            replacement.clone(),
            PairReason::ReplacedHandsOff
        )]
    );
    let stepped_back = format!(
        "SELECT COUNT(*) FROM library_catalog_actions WHERE action_kind = \
         'management_stepped_back' AND local_album_id = '{replacement}' \
         AND reason_code = 'replaced_hands_off'"
    );
    assert_eq!(managed.count(&stepped_back), 1);
    assert_eq!(retired_into(&db, &album), None);
    assert_eq!(
        managed.count(&format!(
            "SELECT (SELECT COUNT(*) FROM library_management_exclusions \
             WHERE local_album_id = '{album}') + (SELECT COUNT(*) FROM library_user_favorites \
             WHERE item_kind = 'album' AND item_id = '{album}')"
        )),
        2,
        "the old album keeps its choice and its star"
    );
    for track in &old {
        assert_eq!(album_row(&db, track), album);
        assert_eq!(where_gone(&db, track).1, "missing");
    }

    let all_actions = "SELECT COUNT(*) FROM library_catalog_actions";
    let actions = managed.count(all_actions);
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert_eq!(
        managed.count(all_actions),
        actions,
        "a second scan writes nothing"
    );
    assert_eq!(corrections.retag_pairs().expect("pairs listed").total, 1);

    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&replacement, "admin")
            .expect("hand-back answers")
    );
    managed.worker_looks().await;
    managed.worker_looks().await;
    assert_ne!(managed.bytes(&new), bytes, "written once handed back");
    rescan(&managed.library).await;
    managed.worker_looks().await;
    assert_eq!(managed.mark(&replacement), None, "not held again");
}

/// A custom edition whose files are retagged by hand and renamed into a
/// folder next to its own: the new album is held and offered as a pair.
/// Merging them retires the old album into the new one, drops the custom
/// edition with a review on the new album, and leaves it held.
#[tokio::test]
async fn a_custom_edition_replaced_by_hand_is_merged_by_a_person() {
    use droppedneedle_library::corrections::models::{PairJoinRequest, PairReason};

    let managed = ManagedLibrary::start("lib-hands-off-custom", |_| {}).await;
    // One file: the fixture's own track number would repeat on a second.
    let (album, old) = managed.add_album("first", 1).await;
    managed.add_album("filler", 1).await;
    managed.activate().await;
    // A custom edition is sealed under a real release group id.
    let group = "aaaaaaaa-0000-4000-8000-000000000001";
    managed
        .db()
        .execute(
            "UPDATE local_album_external_identities SET release_group_mbid = ?2 \
             WHERE local_album_id = ?1",
            [&album, group],
        )
        .expect("group id");
    let mut recall = ambiguous_recall(&managed.library.identify_store, &album);
    for release in &mut recall.releases {
        release.release_group_id = group.to_owned();
    }
    managed.scratch.identify.set_recall(recall);
    crate::common::library::operations(&managed.library)
        .seal_custom_edition(&album, REL_1, managed.edition_revision(&album), "admin")
        .await
        .expect("custom edition sealed");

    let new = managed.retag_by_hand(&old, Some("Retagged")).await;
    let replacement = managed.album_of(&new[0]);
    assert_ne!(replacement, album, "an album row of its own");
    managed.identify_and_confirm(&replacement).await;
    assert_eq!(
        managed.mark(&replacement).as_deref(),
        Some("replaced_hands_off")
    );
    let corrections = crate::common::library::services(&managed.library).corrections;
    let listed = corrections.retag_pairs().expect("pairs listed");
    assert_eq!(listed.pairs.len(), 1);
    let pair = &listed.pairs[0];
    assert_eq!(
        (pair.old_album.id.as_str(), pair.new_album.id.as_str()),
        (album.as_str(), replacement.as_str())
    );
    assert_eq!(pair.reason, PairReason::ReplacedHandsOff);
    assert!(
        pair.old_album
            .edition
            .as_ref()
            .is_some_and(|edition| edition.custom)
    );
    corrections
        .join_retag_pair(&PairJoinRequest {
            old_album_id: album.clone(),
            new_album_id: replacement.clone(),
            keep_edition_of: None,
            expected_identity_revisions: std::collections::BTreeMap::from([
                (album.clone(), pair.old_album.identity_revision),
                (replacement.clone(), pair.new_album.identity_revision),
            ]),
            actor: "admin".to_owned(),
        })
        .expect("joined");

    let db = managed.db();
    assert_eq!(retired_into(&db, &album), Some(replacement.clone()));
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM library_custom_edition_active WHERE local_album_id \
             IN ('{album}', '{replacement}')"
        )),
        0,
        "the custom edition is dropped"
    );
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM library_identify_reviews WHERE local_album_id = \
             '{replacement}' AND state = 'pending' AND reason_code = 'EDITION_CLEARED_CUSTOM'"
        )),
        1,
        "with a review on the album that stays"
    );
    assert_eq!(
        managed.mark(&replacement).as_deref(),
        Some("replaced_hands_off")
    );
    let bytes = managed.bytes(&new);
    managed.look(&replacement).await;
    managed.worker_looks().await;
    assert_eq!(managed.bytes(&new), bytes, "nothing written");
}

// ---------------------------------------------------------------------------
// A file matched to an edition track by hand.
// ---------------------------------------------------------------------------

/// The recording of the third track on the hand-match album's edition.
const THIRD_RECORDING: &str = "c3c3c3c3-3333-4333-8333-333333333333";

/// A three-track album whose third file went missing and whose new file
/// nothing places.
struct HandMatchAlbum {
    album: String,
    /// The release the files name, three tracks long.
    fit: Release,
    /// The files of tracks 1 and 2, and the third track's file that went.
    tracks: Vec<String>,
    /// The file that came instead: no recording or track id, an odd title.
    odd: String,
}

/// A three-file album in `music/{folder}`, identified as the release its
/// tags name and chosen by the owner. A listener plays and stars its third
/// track; then that file goes, and a file with the album's release ids but
/// no recording or track id and an odd title comes in its place.
async fn hand_match_album(rig: &Rig, folder: &str) -> HandMatchAlbum {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let app = &rig.app;
    let music = app.config.root_app_dir.join("music");
    plant(&music, &format!("{folder}/01.flac"), "flac_full_01.flac");
    plant(&music, &format!("{folder}/02.flac"), "flac_full_02.flac");
    let third = plant(&music, &format!("{folder}/03.flac"), "flac_full_02.flac");
    save_tags(
        &third,
        &[
            TagEdit::verbatim(TagField::Title, vec!["Track 3".to_owned()]),
            TagEdit::verbatim(TagField::TrackNumber, vec!["3".to_owned()]),
            TagEdit::verbatim(
                TagField::MusicBrainzRecordingId,
                vec![THIRD_RECORDING.to_owned()],
            ),
            TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
        ],
    )
    .expect("third track tagged");
    let paused_at = rig.pause_identification().await;
    app.add_library_folder("music", &music).await;
    let tracks: Vec<String> = (1..=3)
        .map(|number| {
            rig.scan_store
                .track_at("music", &format!("{folder}/{number:02}.flac"))
                .unwrap()
                .expect("track indexed")
        })
        .collect();
    let album = rig
        .scan_store
        .album_for_track(&tracks[0])
        .unwrap()
        .expect("album");
    for track in &tracks {
        assert_eq!(
            rig.scan_store.album_for_track(track).unwrap().as_ref(),
            Some(&album)
        );
    }
    let fit = tagged_release(&rig.identify_store, &album);
    assert_eq!(fit.tracks.len(), 3);
    rig.identify.set_recall(RecallResult {
        releases: vec![fit.clone()],
        ..RecallResult::default()
    });
    rig.resume_identification(paused_at).await;
    rig.identified(&album, 1).await;
    let auth = app.login_admin().await;
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/library/albums/{album}/edition"),
            &[("authorization", auth.as_str())],
            Some(json!({"release_mbid": fit.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let listener = app.login_user("listener").await.user_id;
    app.db()
        .execute_batch(&format!(
            "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('{listener}', 'track', '{}', 0); \
             INSERT INTO library_play_history (id, user_id, local_track_id, track_name, \
             artist_name, played_at) VALUES ('play-3', '{listener}', '{}', 'Track 3', \
             'Artist', '2026-01-01T00:00:00Z');",
            tracks[2], tracks[2]
        ))
        .expect("play and star seeded");

    std::fs::remove_file(&third).expect("third file goes");
    let odd = plant(&music, &format!("{folder}/odd.flac"), "flac_full_02.flac");
    save_tags(
        &odd,
        &[
            TagEdit::verbatim(TagField::Title, vec!["Untitled Take".to_owned()]),
            TagEdit::verbatim(TagField::TrackNumber, vec!["9".to_owned()]),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseTrackId, Vec::new()),
        ],
    )
    .expect("odd file tagged");
    app.scan_library().await;
    let odd = rig
        .scan_store
        .track_at("music", &format!("{folder}/odd.flac"))
        .unwrap()
        .expect("odd file indexed");
    assert_eq!(
        rig.scan_store.album_for_track(&odd).unwrap().as_ref(),
        Some(&album)
    );
    let db = app.db();
    app.until("the album settles after the scan", || {
        let busy: i64 = db
            .query_row(
                "SELECT (SELECT COUNT(*) FROM library_identify_jobs \
                 WHERE state IN ('queued', 'running')) \
                 + (SELECT COUNT(*) FROM library_edition_remap_queue)",
                [],
                |row| row.get(0),
            )
            .expect("queues read");
        async move { (busy == 0).then_some(()) }
    })
    .await;
    HandMatchAlbum {
        album,
        fit,
        tracks,
        odd,
    }
}

/// One track identity row, if the file has one.
fn identity_of(db: &rusqlite::Connection, track: &str) -> Option<TrackRow> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT recording_mbid, release_mbid, release_track_mbid, medium_position, \
         release_track_position, decision_source FROM local_track_external_identities \
         WHERE local_track_id = ?1",
        [track],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )
    .optional()
    .expect("identity reads")
}

/// The album's identity row: source, chooser and revision.
fn album_choice(db: &rusqlite::Connection, album: &str) -> (String, Option<String>, i64) {
    db.query_row(
        "SELECT decision_source, selected_by_user_id, row_revision \
         FROM local_album_external_identities WHERE local_album_id = ?1",
        [album],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("album identity reads")
}

fn hand_match_row(db: &rusqlite::Connection, track: &str) -> Option<(String, String, bool)> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT release_mbid, release_track_mbid, matched_by_user_id IS NOT NULL \
         FROM library_track_hand_matches WHERE local_track_id = ?1",
        [track],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .expect("hand matches read")
}

fn album_match_state(db: &rusqlite::Connection, album: &str) -> Option<String> {
    use rusqlite::OptionalExtension as _;
    db.query_row(
        "SELECT state FROM library_album_match_state WHERE local_album_id = ?1",
        [album],
        |row| row.get(0),
    )
    .optional()
    .expect("match state reads")
}

/// One management job as the operations page reads it.
async fn job_view(app: &TestApp, headers: &[(&str, &str)], job: &str) -> Value {
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/library/management/previews/{job}"),
            headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Wait for a management preview to be planned, apply it, and wait for
/// the write to succeed.
async fn run_management_job(app: &TestApp, headers: &[(&str, &str)], job: &str, token: &Value) {
    let ready = app
        .until("the preview is planned", || async {
            let view = job_view(app, headers, job).await;
            view["ready_for_confirmation"].as_bool()?.then_some(view)
        })
        .await;
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/management/previews/{job}/apply"),
            headers,
            Some(json!({
                "preview_token": token,
                "expected_operation_row_revision": ready["operation_row_revision"],
                "idempotency_key": format!("apply-{job}"),
                "confirmation": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let state = app
        .until("the write ends", || async {
            let view = job_view(app, headers, job).await;
            let state = view["state"].as_str()?.to_owned();
            (!matches!(state.as_str(), "queued" | "running" | "ready")).then_some(state)
        })
        .await;
    assert_eq!(state, "succeeded");
}

/// One file of a plan, by id.
fn plan_file<'a>(plan: &'a Value, track: &str) -> &'a Value {
    plan["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["local_track_id"] == json!(track))
        .expect("file in the plan")
}

/// A curator matches a file nothing could place to the edition track whose
/// file went missing. The file then sits on that track as the person's
/// (the plan shows it matched by hand, the recycle bin refuses it), the
/// missing track's play and star move to it, the album is queued for
/// management and is not marked taken over. A scan and choosing the same
/// edition again keep the match. A tag edit written to the file after the
/// match holds "Undo match" back until that write is undone from History;
/// then "Undo match" puts the file back as it was, and the play and star
/// stay. On an edition the app picked, the match makes the edition the
/// person's, and the edition's own undo puts the album and the file back.
/// One carried from v2 already is a person's and keeps its source.
#[tokio::test]
async fn a_file_matched_by_hand_counts_as_its_track() {
    let rig = Rig::start("lib-hand-match").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let h = hand_match_album(&rig, "album-h").await;
    let db = app.db();
    let album = &h.album;
    let plan_uri = format!("/api/v3/library/albums/{album}/edition/plan");
    let file_uri = format!("/api/v3/library/albums/{album}/edition/files/{}", h.odd);
    let third = h.fit.tracks[2].id.clone();
    let plan = |uri: String| async move {
        let (status, body) = app.call("GET", &uri, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };

    let before = plan(plan_uri.clone()).await;
    assert_ne!(
        plan_file(&before, &h.odd)["fate"],
        json!("keep"),
        "{before}"
    );
    assert_eq!(
        before["tracks"][2]["local_track_id"],
        Value::Null,
        "{before}"
    );
    let prior = identity_of(&db, &h.odd);
    let release: String = db
        .query_row(
            "SELECT release_mbid FROM local_album_external_identities WHERE local_album_id = ?1",
            [album],
            |row| row.get(0),
        )
        .expect("album release");
    db.execute(
        "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1",
        [album],
    )
    .expect("queue cleared");
    let carries = continuations(&db);

    let (status, body) = app
        .call(
            "PUT",
            &file_uri,
            &headers,
            Some(json!({
                "release_track_mbid": third,
                "expected_identity_revision": before["identity_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let matched = identity_of(&db, &h.odd);
    assert_eq!(
        matched,
        Some((
            THIRD_RECORDING.to_owned(),
            Some(release.clone()),
            Some(third.clone()),
            Some(1),
            Some(3),
            "manual".to_owned(),
        ))
    );
    assert_eq!(
        hand_match_row(&db, &h.odd),
        Some((release.clone(), third.clone(), true))
    );
    let after = plan(plan_uri.clone()).await;
    let file = plan_file(&after, &h.odd);
    assert_eq!(
        (
            &file["fate"],
            &file["edition_position"],
            &file["matched_by_hand"],
            &file["match_written"]
        ),
        (&json!("keep"), &json!(3), &json!(true), &json!(false)),
        "{after}"
    );
    assert_eq!(after["missing_count"], json!(0), "{after}");
    assert_eq!(holdings(&db, &h.odd)[..2], [1, 1], "play and star moved");
    assert_eq!(holdings(&db, &h.tracks[2])[..2], [0, 0]);
    assert_eq!(continuations(&db), carries + 1);
    let count = |sql: &str| -> i64 {
        db.query_row(sql, [album], |row| row.get(0))
            .expect("count reads")
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM library_management_automatic_queue WHERE local_album_id = ?1"),
        1,
        "queued for management"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = ?1"),
        0,
        "not taken over"
    );
    assert_eq!(catalog_actions(&db, album, "track_matched"), 1);

    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album}/edition/extras/remove"),
            &headers,
            Some(json!({"track_ids": [h.odd]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("FILE_KEEPS_ITS_PLACE"));

    // A scan, then the same edition chosen again: the match stays.
    app.scan_library().await;
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/library/albums/{album}/edition"),
            &headers,
            Some(json!({"release_mbid": h.fit.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(identity_of(&db, &h.odd), matched);
    assert_eq!(
        plan_file(&plan(plan_uri.clone()).await, &h.odd)["matched_by_hand"],
        json!(true)
    );

    // A tag edit written to the file after the match: Undo match waits
    // until that write is undone from History.
    let page = "/api/v3/settings/library-management";
    let (status, saved) = app.call("GET", page, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let mut settings = saved.clone();
    let fields = settings.as_object_mut().expect("settings");
    fields.remove("settings_revision");
    fields.insert(
        "default_profile_id".to_owned(),
        json!(droppedneedle_library::management::settings::PICARD_ORGANIZER_PROFILE_ID),
    );
    let (status, body) = app
        .call(
            "PUT",
            page,
            &headers,
            Some(json!({
                "settings": settings,
                "expected_settings_revision": saved["settings_revision"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, editor) = app
        .call(
            "GET",
            &format!("/api/v3/library/management/tracks/{}/tag-editor", h.odd),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{editor}");
    let (status, edit) = app
        .call(
            "POST",
            "/api/v3/library/management/tag-edit-previews",
            &headers,
            Some(json!({
                "local_track_id": h.odd,
                "mode": "save_override",
                "expected_settings_revision": editor["settings_revision"],
                "expected_policy_revision": editor["policy_revision"],
                "fields": [{"field_name": "title", "value": "Untitled Take (edited)"}],
                "idempotency_key": "hand-match-edit",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edit}");
    let edit_job = edit["job_id"].as_str().expect("edit job").to_owned();
    run_management_job(app, &headers, &edit_job, &edit["preview_token"]).await;
    assert_eq!(
        plan_file(&plan(plan_uri.clone()).await, &h.odd)["match_written"],
        json!(true)
    );
    let (status, body) = app.call("DELETE", &file_uri, &headers, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("HAND_MATCH_WRITTEN"));
    let written = job_view(app, &headers, &edit_job).await;
    let (status, undo) = app
        .call(
            "POST",
            &format!("/api/v3/library/management/operations/{edit_job}/undo-preview"),
            &headers,
            Some(json!({
                "expected_operation_row_revision": written["operation_row_revision"],
                "idempotency_key": "hand-match-edit-undo",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    let undo_job = undo["job_id"].as_str().expect("undo job").to_owned();
    run_management_job(app, &headers, &undo_job, &undo["preview_token"]).await;

    // Undo match: the file is back as it was; the play and star stay.
    let (status, body) = app.call("DELETE", &file_uri, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(identity_of(&db, &h.odd), prior);
    assert_eq!(hand_match_row(&db, &h.odd), None);
    let undone = plan(plan_uri.clone()).await;
    let file = plan_file(&undone, &h.odd);
    assert_ne!(file["fate"], json!("keep"), "{undone}");
    assert_eq!(file["matched_by_hand"], json!(false));
    assert_eq!(holdings(&db, &h.odd)[..2], [1, 1], "play and star stay");
    let (status, body) = app.call("DELETE", &file_uri, &headers, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("NO_HAND_MATCH"));

    // An edition the app picked, left as a best guess.
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET decision_source = 'automatic', \
         selected_by_user_id = NULL, row_revision = row_revision + 1 \
         WHERE local_album_id = '{album}'; \
         UPDATE local_track_external_identities SET decision_source = 'automatic' \
         WHERE local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = '{album}'); \
         INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, candidates_json, updated_at) \
         VALUES ('{album}', 'unconfirmed', 'CLOSE_CALL', '{release}', '[]', 1);"
    ))
    .expect("the app's best guess");
    let guessed = album_choice(&db, album);
    let before_guess = identity_of(&db, &h.odd);
    let revision = plan(plan_uri.clone()).await["identity_revision"].clone();
    let (status, body) = app
        .call(
            "PUT",
            &file_uri,
            &headers,
            Some(json!({"release_track_mbid": third, "expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("chosen"), "{body}");
    let chosen = album_choice(&db, album);
    assert_eq!(chosen.0, "manual");
    assert!(chosen.1.is_some(), "chosen by the curator");
    assert_eq!(album_match_state(&db, album), None);
    assert_eq!(catalog_actions(&db, album, "edition_confirmed"), 1);

    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/library/albums/{album}/edition/undo"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let back = album_choice(&db, album);
    assert_eq!((back.0.as_str(), &back.1), ("automatic", &guessed.1));
    assert_eq!(
        album_match_state(&db, album).as_deref(),
        Some("unconfirmed")
    );
    assert_eq!(identity_of(&db, &h.odd), before_guess);
    assert_eq!(
        plan_file(&plan(plan_uri.clone()).await, &h.odd)["matched_by_hand"],
        json!(false)
    );

    // An edition carried from v2.
    db.execute_batch(&format!(
        "UPDATE local_album_external_identities SET decision_source = 'legacy_import', \
         selected_by_user_id = NULL, row_revision = row_revision + 1 \
         WHERE local_album_id = '{album}'; \
         DELETE FROM library_album_match_state WHERE local_album_id = '{album}';"
    ))
    .expect("v2's edition");
    let revision = plan(plan_uri.clone()).await["identity_revision"].clone();
    let (status, body) = app
        .call(
            "PUT",
            &file_uri,
            &headers,
            Some(json!({"release_track_mbid": third, "expected_identity_revision": revision})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let carried = album_choice(&db, album);
    assert_eq!(carried.0, "legacy_import", "v2's choice stays as it is");
    assert!(carried.1.is_none());
    rig.stop().await;
}

/// What a refused match must leave alone: the album's identity, its files'
/// rows, the record of matches, the takeover mark and the audit rows.
fn hand_match_world(db: &rusqlite::Connection, album: &str) -> (String, Vec<String>, [i64; 3]) {
    let identity: String = db
        .query_row(
            "SELECT decision_source || '|' || COALESCE(release_mbid, '') || '|' || row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1",
            [album],
            |row| row.get(0),
        )
        .expect("album identity reads");
    let rows = db
        .prepare(
            "SELECT t.id || '|' || COALESCE(i.release_mbid, '') || '|' || \
             COALESCE(i.release_track_mbid, '') || '|' || COALESCE(i.decision_source, '') || \
             '|' || COALESCE(i.row_revision, 0) FROM local_tracks t \
             LEFT JOIN local_track_external_identities i ON i.local_track_id = t.id \
             WHERE t.local_album_id = ?1 ORDER BY t.id",
        )
        .expect("rows prepare")
        .query_map([album], |row| row.get(0))
        .expect("rows read")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("rows decode");
    let counts = [
        "SELECT COUNT(*) FROM library_track_hand_matches h \
         JOIN local_tracks t ON t.id = h.local_track_id WHERE t.local_album_id = ?1",
        "SELECT COUNT(*) FROM library_album_takeovers WHERE local_album_id = ?1",
        "SELECT COUNT(*) FROM library_catalog_actions WHERE local_album_id = ?1",
    ]
    .map(|sql| {
        db.query_row(sql, [album], |row| row.get(0))
            .expect("counts read")
    });
    (identity, rows, counts)
}

/// Every refusal of a match by hand, each with nothing changed: a stale
/// revision; a track another file holds by its stored row, and its twin, a
/// track the matcher places another file on that has no stored row; a file
/// already on a track; another album's file and a missing file; a track
/// not on the edition; a custom edition; "Leave unmanaged". A track only a
/// missing file holds can be taken. Its undo is refused once another
/// edition was chosen, and the record stays.
#[tokio::test]
async fn a_match_by_hand_is_refused_with_nothing_changed() {
    let rig = Rig::start("lib-hand-match-refusals").await;
    let app = &rig.app;
    let auth = app.login_admin().await;
    let headers = [("authorization", auth.as_str())];
    let h = hand_match_album(&rig, "album-r").await;
    let music = app.config.root_app_dir.join("music");
    plant(&music, "elsewhere/01.flac", "flac_cjk_01.flac");
    app.scan_library().await;
    let elsewhere = rig
        .scan_store
        .track_at("music", "elsewhere/01.flac")
        .unwrap()
        .expect("another album's file");
    assert_ne!(
        rig.scan_store.album_for_track(&elsewhere).unwrap().as_ref(),
        Some(&h.album)
    );
    let db = app.db();
    let album = &h.album;
    let edition = format!("/api/v3/library/albums/{album}/edition");
    let revision = || async {
        let (status, body) = app.call("GET", &edition, &headers, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["identity_revision"].as_i64().expect("revision")
    };
    let put = |track: String, release_track: String, expected: i64| async move {
        app.call(
            "PUT",
            &format!("/api/v3/library/albums/{album}/edition/files/{track}"),
            &headers,
            Some(json!({
                "release_track_mbid": release_track,
                "expected_identity_revision": expected,
            })),
        )
        .await
    };
    let tracks = |n: usize| h.fit.tracks[n].id.clone();
    // The second file loses its stored row: the matcher still places it.
    db.execute(
        "DELETE FROM local_track_external_identities WHERE local_track_id = ?1",
        [&h.tracks[1]],
    )
    .expect("second file's row gone");

    let current = revision().await;
    let refusals: [(&str, String, String, i64, StatusCode, &str); 7] = [
        (
            "a stale revision",
            h.odd.clone(),
            tracks(2),
            current - 1,
            StatusCode::CONFLICT,
            "STALE_REVISION",
        ),
        (
            "a track another file holds",
            h.odd.clone(),
            tracks(0),
            current,
            StatusCode::CONFLICT,
            "EDITION_TRACK_TAKEN",
        ),
        (
            "a track the matcher gives another file",
            h.odd.clone(),
            tracks(1),
            current,
            StatusCode::CONFLICT,
            "EDITION_TRACK_TAKEN",
        ),
        (
            "a file already on a track",
            h.tracks[0].clone(),
            tracks(2),
            current,
            StatusCode::CONFLICT,
            "FILE_ALREADY_PLACED",
        ),
        (
            "another album's file",
            elsewhere.clone(),
            tracks(2),
            current,
            StatusCode::NOT_FOUND,
            "FILE_NOT_ON_ALBUM",
        ),
        (
            "a missing file",
            h.tracks[2].clone(),
            tracks(2),
            current,
            StatusCode::NOT_FOUND,
            "FILE_NOT_ON_ALBUM",
        ),
        (
            "a track not on the edition",
            h.odd.clone(),
            "not-a-track".to_owned(),
            current,
            StatusCode::BAD_REQUEST,
            "TRACK_NOT_ON_EDITION",
        ),
    ];
    for (case, track, release_track, expected, want, code) in refusals {
        let world = hand_match_world(&db, album);
        let (status, body) = put(track, release_track, expected).await;
        assert_eq!(status, want, "{case}: {body}");
        assert_eq!(body["error"]["code"], json!(code), "{case}: {body}");
        assert_eq!(
            hand_match_world(&db, album),
            world,
            "{case}: nothing changed"
        );
    }

    // A custom edition, then "Leave unmanaged": each undone afterwards.
    for (route, want, code) in [
        ("custom", StatusCode::CONFLICT, "CUSTOM_EDITION_KEEPS_FILES"),
        (
            "leave-unmanaged",
            StatusCode::BAD_REQUEST,
            "NO_EDITION_TO_PLAN",
        ),
    ] {
        let body = json!({
            "release_mbid": h.fit.id,
            "expected_identity_revision": revision().await,
        });
        let (status, sealed) = app
            .call("POST", &format!("{edition}/{route}"), &headers, Some(body))
            .await;
        assert_eq!(status, StatusCode::OK, "{route}: {sealed}");
        let world = hand_match_world(&db, album);
        let (status, refused) = put(h.odd.clone(), tracks(2), revision().await).await;
        assert_eq!(status, want, "{route}: {refused}");
        assert_eq!(refused["error"]["code"], json!(code), "{route}: {refused}");
        assert_eq!(
            hand_match_world(&db, album),
            world,
            "{route}: nothing changed"
        );
        let (status, undone) = app
            .call("POST", &format!("{edition}/undo"), &headers, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{route}: {undone}");
    }

    // A track only a missing file holds is free.
    let (status, body) = put(h.odd.clone(), tracks(2), revision().await).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let file_uri = format!("{edition}/files/{}", h.odd);

    // Another edition chosen since: the match no longer stands.
    let mut other = h.fit.clone();
    other.id = REL_2.to_owned();
    rig.identify.set_recall(RecallResult {
        releases: vec![h.fit.clone(), other.clone()],
        ..RecallResult::default()
    });
    let (status, body) = app
        .call(
            "PUT",
            &edition,
            &headers,
            Some(json!({"release_mbid": other.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let world = hand_match_world(&db, album);
    let (status, body) = app.call("DELETE", &file_uri, &headers, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("STALE_REVISION"));
    assert_eq!(hand_match_world(&db, album), world, "nothing changed");
    assert!(hand_match_row(&db, &h.odd).is_some(), "the record stays");
    rig.stop().await;
}

/// Rewrite the total-sample count in a FLAC file's STREAMINFO so the file
/// reads `seconds` longer. The frames stay as they were.
fn lengthen_flac(path: &Path, seconds: f64) {
    let mut bytes = std::fs::read(path).expect("file reads");
    let stream = bytes
        .windows(4)
        .position(|window| window == b"fLaC")
        .expect("a FLAC stream");
    // After the marker and the block header: block and frame sizes (10
    // bytes), then sample rate (20 bits), channels (3), bits per sample
    // (5) and total samples (36).
    let at = stream + 8 + 10;
    let packed = u64::from_be_bytes(bytes[at..at + 8].try_into().expect("eight bytes"));
    let samples_mask = (1u64 << 36) - 1;
    let rate = packed >> 44;
    let samples = (packed & samples_mask) + (seconds * rate as f64) as u64;
    let packed = (packed & !samples_mask) | samples;
    bytes[at..at + 8].copy_from_slice(&packed.to_be_bytes());
    std::fs::write(path, bytes).expect("file writes");
}

/// A track re-ripped over the file the app wrote, with its title edited
/// by hand: its audio changed, so it is a new copy of the same recording,
/// not a hand edit. The scan forgets the app's record and says so in the
/// album's history; automatic management writes the edition into the
/// copy, and undoing that run gives the copy back as it came.
#[tokio::test]
async fn a_re_ripped_track_is_managed_again_and_undo_gives_it_back() {
    use droppedneedle_library::operations::history::{HistoryKind, album_history};
    use droppedneedle_library::publish::record::record;
    use droppedneedle_library::tags::read::read_scan_metadata;
    use droppedneedle_library::tags::{AudioFormat, TagField, read_fields, save_tags};

    let managed = ManagedLibrary::start("lib-new-copy", |_| {}).await;
    let (album, tracks) = managed.add_album("first", 2).await;
    managed.activate().await;
    managed.look(&album).await;
    let track = &tracks[0];
    let has_record = |track: &str| record(&managed.db(), track).expect("record reads").exists();
    assert!(
        tracks.iter().all(|track| has_record(track)),
        "the app wrote both files"
    );
    // The scan reads the app's write, which named the album anew.
    rescan(&managed.library).await;
    assert!(
        tracks.iter().all(|track| has_record(track)),
        "the app's own write"
    );
    let title = |path: &Path| read_fields(path).expect("tags read")[&TagField::Title].clone();

    let file = managed.path(track);
    let before = read_scan_metadata(&file, AudioFormat::Flac)
        .expect("reads")
        .1
        .duration_seconds
        .expect("a length");
    let rip = managed.scratch.join("rip.flac");
    std::fs::copy(&file, &rip).expect("copied to scratch");
    save_tags(&rip, &[title_edit("First 1 (rip)")]).expect("retagged");
    lengthen_flac(&rip, 2.0);
    let after = read_scan_metadata(&rip, AudioFormat::Flac)
        .expect("reads")
        .1
        .duration_seconds
        .expect("a length");
    assert!((after - before - 2.0).abs() < 0.01, "{before} -> {after}");
    std::fs::copy(&rip, &file).expect("the copy put at the track's path");
    managed
        .db()
        .execute("DELETE FROM library_management_automatic_queue", [])
        .expect("queue emptied");
    rescan(&managed.library).await;

    assert!(!has_record(track), "the record is forgotten");
    assert!(has_record(&tracks[1]));
    let actions = "SELECT COUNT(*) FROM library_catalog_actions \
                   WHERE action_kind = 'new_copy_at_path'";
    assert_eq!(managed.count(actions), 1);
    let named: (String, String, f64, f64) = managed
        .db()
        .query_row(
            "SELECT a.local_album_id, json_extract(t.value, '$.track_id'), \
             json_extract(t.value, '$.before.seconds'), json_extract(t.value, '$.after.seconds') \
             FROM library_catalog_actions a, json_each(a.after_json, '$.tracks') t \
             WHERE a.action_kind = 'new_copy_at_path' AND a.reason_code = 'NEW_COPY_AT_PATH'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("one track named");
    assert_eq!(named, (album.clone(), track.clone(), before, after));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs_f64();
    let told: Vec<(HistoryKind, usize, Vec<String>)> =
        album_history(&managed.db(), &album, "admin", now)
            .expect("history reads")
            .expect("album live")
            .into_iter()
            .map(|entry| (entry.kind, entry.track_count, entry.track_titles))
            .collect();
    assert_eq!(
        told,
        vec![(
            HistoryKind::TracksNewCopy,
            1,
            vec!["First 1 (rip)".to_owned()]
        )]
    );
    assert_eq!(
        managed.count(&format!(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
             WHERE local_album_id = '{album}' AND not_before < 1e15"
        )),
        1,
        "the album is queued"
    );

    let runs = managed.automatic_runs().len();
    managed.worker_looks().await;
    assert_eq!(managed.mark(&album), None);
    let all = managed.automatic_runs();
    assert_eq!(all.len(), runs + 1, "one new run");
    assert_eq!(all.last().expect("a run").0, "succeeded");
    let file = managed.path(track);
    assert_eq!(title(&file), vec!["first 1".to_owned()]);
    let written = managed.recorded(track);
    assert!(written.is_some(), "the app recorded the bytes it placed");

    rescan(&managed.library).await;
    assert_eq!(
        managed.count(actions),
        1,
        "the app's own write is no new copy"
    );
    assert_eq!(managed.recorded(track), written);

    let management = managed.management();
    let (history, _) = management.history(Default::default()).expect("history");
    let run = history
        .iter()
        .find(|item| item.origin == "scan_discovered")
        .expect("the run")
        .job
        .id
        .clone();
    let detail = management.detail(&run).expect("detail");
    let undo = management
        .undo_preview(&run, detail.job.row_revision, "undo-new-copy", "admin")
        .expect("undo preview");
    assert!(
        management_step(&managed.library).await,
        "undo planning runs"
    );
    let undone = management_apply(&managed.library, &undo).await;
    assert_eq!(undone.job.state.as_str(), "succeeded");
    assert_eq!(
        title(&managed.path(track)),
        vec!["First 1 (rip)".to_owned()]
    );
    assert_eq!(managed.mark(&album).as_deref(), Some("run_undone"));
}

/// A library carried from v2: two albums matched to exact releases whose
/// documents the upgrade did not bring, their artist linked by v2. The
/// first sweep after the start keeps the link. Once identification runs,
/// an idle pass fetches each missing document once: the one MusicBrainz
/// has proves the artist on its album, and the album whose release it no
/// longer has is offered for identification again.
#[tokio::test]
async fn a_carried_library_fetches_its_release_documents_and_keeps_its_links() {
    use droppedneedle_library::identify::sources::ReleaseAnswer;

    const ARTIST: &str = "33333333-3333-4333-8333-333333333333";
    const GROUP: &str = "44444444-4444-4444-8444-444444444444";
    let rig = Rig::start("lib-carried-documents").await;
    let paused = rig.pause_identification().await;
    let music = rig.app.config.root_app_dir.join("music");
    plant(&music, "a/01.flac", "flac_full_01.flac");
    plant(&music, "b/01.flac", "flac_full_01.flac");
    rig.app.add_library_folder("music", &music).await;
    let store = &rig.scan_store;
    let track_a = store.track_at("music", "a/01.flac").unwrap().expect("a/01");
    let track_b = store.track_at("music", "b/01.flac").unwrap().expect("b/01");
    let album_a = store.album_for_track(&track_a).unwrap().expect("album a");
    let album_b = store.album_for_track(&track_b).unwrap().expect("album b");
    let db = rig.app.db();
    let text = |sql: &str| texts(&db, sql);
    let artist = text(&format!(
        "SELECT album_artist_id FROM local_albums WHERE id = '{album_a}'"
    ))[0]
        .clone();
    let credited = text(&format!(
        "SELECT COALESCE(credited_name, '') FROM local_album_artists \
         WHERE local_album_id = '{album_a}' AND position = 0"
    ))[0]
        .clone();
    drop(db);

    // The rows as the upgrade leaves them: matches, track mappings and the
    // artist's link carried, no release document, proof or queued job, and
    // no file artist ids on the carried credits.
    let Rig {
        app,
        identify,
        contrib,
        scan_store,
        identify_store,
        settled_at,
    } = rig;
    let (app, ()) = app
        .while_stopped(|config| {
            let db = droppedneedle_kernel::db::open_connection(&config.library_db_path)
                .expect("database opens");
            db.execute_batch(&format!(
                "DELETE FROM local_album_external_identities; \
                 DELETE FROM local_track_external_identities; \
                 DELETE FROM library_album_match_state; \
                 DELETE FROM library_identify_jobs; \
                 DELETE FROM library_artist_credit_proofs; \
                 DELETE FROM library_artist_reconciliation_state; \
                 DELETE FROM library_management_metadata_snapshots; \
                 DELETE FROM local_artist_external_identities; \
                 UPDATE local_album_artists SET artist_mbid = NULL; \
                 UPDATE local_track_artists SET artist_mbid = NULL; \
                 INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
                 ('{album_a}', 'musicbrainz', '{GROUP}', '{REL_1}', 'automatic', 1), \
                 ('{album_b}', 'musicbrainz', '{GROUP}', '{REL_2}', 'automatic', 1); \
                 INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_mbid, release_track_mbid, decision_source, \
                 selected_at) VALUES \
                 ('{track_a}', 'musicbrainz', 'rec-1', '{REL_1}', 'rt-1', 'automatic', 1), \
                 ('{track_b}', 'musicbrainz', 'rec-1', '{REL_2}', 'rt-1', 'automatic', 1); \
                 INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                 provider_artist_id, decision_source, selected_at) \
                 VALUES ('{artist}', 'musicbrainz', '{ARTIST}', 'automatic', 1);"
            ))
            .expect("carried rows seed");
        })
        .await;
    let rig = Rig {
        app,
        identify,
        contrib,
        scan_store,
        identify_store,
        settled_at,
    };
    let db = rig.app.db();
    let link = format!(
        "SELECT provider_artist_id || ' ' || decision_source \
         FROM local_artist_external_identities WHERE local_artist_id = '{artist}'"
    );
    let reasons = format!(
        "SELECT local_album_id || ' ' || state || ' ' || reason_code \
         FROM library_artist_reconciliation_state \
         WHERE local_album_id IN ('{album_a}', '{album_b}') ORDER BY local_album_id = '{album_b}'"
    );
    let waiting = vec![
        format!("{album_a} waiting_for_identity RELEASE_DOCUMENT_MISSING"),
        format!("{album_b} waiting_for_identity RELEASE_DOCUMENT_MISSING"),
    ];
    rig.app
        .until("the first sweep looks at both albums", || {
            let done = texts(&db, &reasons) == waiting;
            async move { done.then_some(()) }
        })
        .await;
    assert_eq!(texts(&db, &link), [format!("{ARTIST} automatic")]);

    let credit = CreditedArtist {
        id: ARTIST.to_owned(),
        name: credited.clone(),
        sort_name: None,
        join: String::new(),
    };
    rig.identify.set_document(
        REL_1,
        ReleaseAnswer::Found(Box::new(Release {
            id: REL_1.to_owned(),
            release_group_id: GROUP.to_owned(),
            title: "Album".to_owned(),
            artists: vec![credit.clone()],
            tracks: vec![ReleaseTrack {
                id: "rt-1".to_owned(),
                recording_id: "rec-1".to_owned(),
                title: "Song 1".to_owned(),
                disc: 1,
                position: 1,
                absolute_position: 1,
                artists: vec![credit],
                ..ReleaseTrack::default()
            }],
            ..Release::default()
        })),
    );
    rig.identify.set_document(REL_2, ReleaseAnswer::Missing);
    rig.resume_identification(paused).await;
    let proved = format!("{album_a} projected CREDITS_LINE_UP");
    let offered = format!(
        "SELECT local_album_id FROM library_identify_jobs WHERE local_album_id = '{album_b}'"
    );
    rig.app
        .until(
            "the documents are fetched, the album proved and the other offered",
            || {
                let done = texts(&db, &reasons).first() == Some(&proved)
                    && !texts(&db, &offered).is_empty();
                async move { done.then_some(()) }
            },
        )
        .await;
    assert_eq!(texts(&db, &link), [format!("{ARTIST} automatic")]);
    assert_eq!(
        texts(
            &db,
            &format!(
                "SELECT subject_kind || ' ' || artist_mbid FROM library_artist_credit_proofs \
                 WHERE local_artist_id = '{artist}' ORDER BY subject_kind"
            )
        ),
        [format!("album {ARTIST}"), format!("track {ARTIST}")]
    );
    let finished = format!(
        "SELECT state FROM library_identify_jobs WHERE local_album_id = '{album_b}' \
         AND state NOT IN ('queued', 'running')"
    );
    rig.app
        .until("the other album is identified again", || {
            let done = !texts(&db, &finished).is_empty();
            async move { done.then_some(()) }
        })
        .await;
    assert_eq!(
        texts(&db, &reasons).get(1),
        Some(&format!("{album_b} waiting_for_identity RELEASE_NOT_FOUND"))
    );
    let asked = rig.identify.documents_asked();
    assert_eq!(
        asked.iter().filter(|id| *id == REL_1).count(),
        1,
        "{asked:?}"
    );
    assert_eq!(
        asked.iter().filter(|id| *id == REL_2).count(),
        1,
        "{asked:?}"
    );
    rig.stop().await;
}
