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

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::common::library::ScriptedIdentify;
use crate::common::{ScratchDir, TestApp, closed_outbound};
use axum::http::StatusCode;
use droppedneedle::bootstrap::Port;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::library::contrib::ContribProviders;
use droppedneedle::library::identify::models::{LocalAlbumFacts, RecallResult};
use droppedneedle::library::identify::sqlite::SqliteIdentifyStore;
use droppedneedle::library::matching::{CreditedArtist, Release, ReleaseTrack};
use droppedneedle::library::scan::SqliteScanStore;
use droppedneedle::library::wiring::{LibrarySetup, TestContribProviders};
use droppedneedle::runtime_config::{ConfigStore, Crypto};
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
    contrib: TestContribProviders,
    scan_store: SqliteScanStore,
    identify_store: SqliteIdentifyStore,
    /// When the last identification job the journey accounted for ended.
    settled_at: Cell<i64>,
}

impl Rig {
    async fn start(tag: &str) -> Self {
        let identify = ScriptedIdentify::new();
        let contrib = TestContribProviders::default();
        let mut outbound = closed_outbound();
        outbound.identify_providers = Port::given(identify.clone());
        outbound.contribution_providers = Port::given(ContribProviders {
            discogs: contrib.discogs.clone(),
            musicbrainz: contrib.musicbrainz.clone(),
        });
        let app = TestApp::start_with(tag, outbound).await;
        let db = &app.config.library_db_path;
        Self {
            scan_store: SqliteScanStore::open(db).expect("scan rows open"),
            identify_store: SqliteIdentifyStore::open(db).expect("identify rows open"),
            app,
            identify,
            contrib,
            settled_at: Cell::new(0),
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

    /// Let identification run again; the loop wakes at once.
    async fn resume_identification(&self, paused_at: u64) {
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

    /// Wait until the identification queue is idle, and check that exactly
    /// `jobs` jobs ended since the journey last asked.
    async fn identified(&self, jobs: i64) {
        let since = self.settled_at.get();
        let db = self.app.db();
        let (ended, latest) = self
            .app
            .until("identification settles", || {
                let (open, ended, latest): (i64, i64, i64) = db
                    .query_row(
                        "SELECT COALESCE(SUM(state IN ('queued', 'running')), 0), \
                         COALESCE(SUM(state NOT IN ('queued', 'running') AND updated_ms > ?1), 0), \
                         COALESCE(MAX(updated_ms), 0) FROM library_identify_jobs",
                        [since],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .expect("identify queue reads");
                async move { (open == 0 && ended >= jobs).then_some((ended, latest)) }
            })
            .await;
        assert_eq!(ended, jobs, "identification jobs that ran");
        self.settled_at.set(latest);
    }

    /// Wait until the app is done with the edition a person chose for the
    /// album. Approving a candidate that names only a release queues the
    /// placing of the album's files for the operations loop. When it gets
    /// there it places them, or hands the album back to automatic choice,
    /// and either way moves the album's identity and the catalog revision:
    /// a preview sealed before then is stale, and an Undo after it is
    /// blocked.
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
            let mtime_ns = droppedneedle::library::scan::mtime_ns_from_metadata(&meta);
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
    library.scan_startup_recovery().await;
    for _ in 0..100 {
        library.supervisor_tick().await;
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
    ambiguous_from(&store.album_facts(album_id).expect("album facts"))
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
    droppedneedle::library::tags::read::read_tag_only(
        path,
        droppedneedle::library::tags::AudioFormat::Flac,
    )
    .expect("tags read")
    .title
}

/// File the pending review an edition correction leaves when it drops an
/// album's editions (a merge that detaches conflicting editions): the
/// dropped editions are the candidates, keyed `group:release`.
/// Identification itself never files one; it always picks a best guess.
fn file_edition_review(store: &SqliteIdentifyStore, album_id: &str) -> String {
    use droppedneedle::library::identify::models::CandidateEvidence;
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
    droppedneedle::library::identify::review::file_review(
        store,
        &id,
        album_id,
        "EDITION_CLEARED_CONFLICT",
        vec![candidate("rg-1", REL_1), candidate("rg-2", REL_2)],
    );
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
    rig.identified(1).await;
    // Purity extends past identify: tag reads and probes wrote zero
    // media bytes too.
    assert_eq!(
        snapshot_tree(&music),
        bytes_before,
        "identify reads wrote files"
    );
    {
        assert!(
            rig.identify_store.album_identity(album_key).is_some(),
            "identification picks a best guess"
        );
    }
    {
        assert!(rig.identify_store.pending_for_album(album_key).is_empty());
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
    rig.identified(1).await;

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
    rig.identified(1).await;
    {
        for track in [&track_a, &track_b] {
            assert_eq!(
                rig.scan_store.album_for_track(track).as_deref(),
                Some(album_key),
                "an organized track stays on its album"
            );
        }
        let identity = rig
            .identify_store
            .album_identity(album_key)
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
    let fields_before = droppedneedle::library::tags::read_fields(&file).expect("fields read");
    app.add_library_folder("music", &music).await;
    let track_id = {
        rig.scan_store
            .track_at("music", "album-b/01.flac")
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
    rig.identified(1).await;
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
    let written = droppedneedle::library::tags::read_fields(&file).expect("fields read");
    let release_field = droppedneedle::library::tags::TagField::MusicBrainzReleaseId;
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
        droppedneedle::library::tags::read_fields(&file).expect("fields read"),
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

/// A trusted user may write an album's edition tags into its files and
/// nothing more: no destination, no tag values of their own, no undo and
/// no restore. An admin's same calls go through.
#[tokio::test]
async fn manage_routes_limit_a_trusted_user_to_a_plain_retag() {
    use droppedneedle::library::tags::{TagField, read_fields};

    let rig = Rig::start("lib-manage-roles").await;
    rig.identify.recall_from(ambiguous_from);
    let app = &rig.app;
    let admin_auth = app.login_admin().await;
    let admin_headers = [("authorization", admin_auth.as_str())];
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/admin/users",
            &admin_headers,
            Some(json!({"username": "tris", "password": "tris-password-1234"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tris_id = body["id"].as_str().expect("user id").to_owned();
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/admin/users/{tris_id}/role"),
            &admin_headers,
            Some(json!({"role": "trusted"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": "tris",
                "password": "tris-password-1234",
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tris_auth = format!("Bearer {}", body["token"].as_str().expect("user token"));
    let tris_headers = [("authorization", tris_auth.as_str())];

    let music = app.config.root_app_dir.join("music");
    let file = plant(&music, "album/01.flac", "management_full.flac");
    app.add_library_folder("music", &music).await;
    let track = rig
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = rig.scan_store.album_for_track(&track).expect("album");
    rig.identified(1).await;
    rig.choose_first_edition(&album).await;
    let fields_before = read_fields(&file).expect("fields read");

    // A second identified album: its file is never part of a preview
    // for the first, whoever asks.
    plant(&music, "other/01.flac", "management_full.flac");
    app.scan_library().await;
    let other_track = rig
        .scan_store
        .track_at("music", "other/01.flac")
        .expect("track indexed");
    let other_album = rig.scan_store.album_for_track(&other_track).expect("album");
    assert_ne!(other_album, album);
    rig.identified(1).await;
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

    let undo = json!({"bundle_id": bundle_id});
    let restore = json!({"track_ids": [track]});
    for (uri, request) in [
        ("/api/v3/library/manage/undo", &undo),
        ("/api/v3/library/manage/baseline/restore", &restore),
    ] {
        let (status, body) = app
            .call("POST", uri, &tris_headers, Some(request.clone()))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
    }
    assert!(file.is_file(), "the retagged file stays where it was");

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
    rig.identified(1).await;
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

/// A bare library bundle over a migrated scratch database, with its
/// music folder (not yet a root).
fn bare_library(tag: &str) -> (ScratchDir, LibrarySetup, PathBuf) {
    let scratch = ScratchDir::new(tag);
    let dir = scratch.to_path_buf();
    let connection =
        droppedneedle::db::open_connection(&dir.join("app.db")).expect("scratch db opens");
    droppedneedle::schema::apply_migrations_blocking(&connection).expect("migrations apply");
    drop(connection);
    let library = open_library(&dir);
    (scratch, library, dir.join("music"))
}

/// A library bundle over the database and settings in `dir`, as a
/// restart would build it.
fn open_library(dir: &Path) -> LibrarySetup {
    let config = Arc::new(
        ConfigStore::open(
            &dir.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
        )
        .expect("config opens"),
    );
    LibrarySetup::for_tests_at(
        droppedneedle::auth::wiring::AuthSetup::for_tests()
            .expect("test auth builds")
            .users,
        Arc::new(UuidGenerator) as Arc<dyn IdGenerator>,
        &dir.join("app.db"),
        config,
    )
    .expect("bundle builds")
}

/// Add `music` as the only root and scan it.
async fn add_music_root(library: &LibrarySetup, music: &Path) {
    library
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            droppedneedle::library::scan::EffectivePolicy::Automatic,
        )
        .expect("root adds");
    drain_scans(library).await;
}

/// Scan every root again and wait for it.
async fn rescan(library: &LibrarySetup) {
    use droppedneedle::library::scan::{ScanKind, ScanRequest, ScanTrigger};
    let registry = library.live_registry();
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
        .expect("track indexed");
    rig.scan_store
        .execute_batch_for_tests(&format!(
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
        rig.scan_store.track_at("music", "after/01.flac"),
        Some(track.clone()),
        "the moved file continues its track"
    );
    let following = rig
        .scan_store
        .query_i64_for_tests(&format!(
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
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks")
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
        .expect("copy a indexed");
    let track_b = rig
        .scan_store
        .track_at("music", "lossy/01.flac")
        .expect("copy b indexed");
    let album_a = rig.scan_store.album_for_track(&track_a).expect("album a");
    let album_b = rig.scan_store.album_for_track(&track_b).expect("album b");
    assert_ne!(album_a, album_b, "copies are separate albums");

    let moved = music.join("archive/01.flac");
    std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&copy_b, &moved).expect("move one copy");
    rig.app.scan_library().await;

    assert_eq!(
        rig.scan_store.track_at("music", "archive/01.flac"),
        Some(track_b.clone()),
        "the moved copy keeps its track"
    );
    assert_eq!(rig.scan_store.album_for_track(&track_b), Some(album_b));
    assert_eq!(rig.scan_store.album_for_track(&track_a), Some(album_a));
    assert!(copy_a.is_file());
    rig.stop().await;
}

/// An album whose files went away in an earlier scan is not handed to a
/// new album that happens to share its names: that one is another
/// release, and the old identity stays where it was.
#[tokio::test]
async fn long_missing_album_is_not_taken_over() {
    use droppedneedle::library::tags::TagField;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};

    let rig = Rig::start("lib-no-takeover").await;
    let music = rig.app.config.root_app_dir.join("music");
    let green = plant(&music, "green/01.flac", "flac_full_02.flac");
    // Something stays, so the walk is not an empty mount point.
    plant(&music, "keep/01.flac", "flac_no_tags.flac");
    rig.app.add_library_folder("music", &music).await;
    let old_track = rig
        .scan_store
        .track_at("music", "green/01.flac")
        .expect("track indexed");
    let old_album = rig.scan_store.album_for_track(&old_track).expect("album");
    std::fs::remove_file(&green).expect("delete");
    rig.app.scan_library().await;
    assert_eq!(rig.scan_store.track_at("music", "green/01.flac"), None);

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
        .expect("new track indexed");
    assert_ne!(new_track, old_track);
    let new_album = rig.scan_store.album_for_track(&new_track).expect("album");
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
        rig.scan_store.track_at("share", "album/01.flac"),
        Some(track.clone()),
        "the share's track keeps its id and stays available"
    );
    let copy = rig
        .scan_store
        .track_at("local", "album/01.flac")
        .expect("copy indexed");
    assert_ne!(copy, track, "the copy gets its own row");
    assert!(
        rig.scan_store.track_at("small", "01.flac").is_some(),
        "an emptied small root keeps its tracks"
    );
    assert_eq!(
        rig.scan_store
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing'")
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
    use droppedneedle::library::tags::TagField;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};

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
        .expect("track indexed");
    let album = rig.scan_store.album_for_track(&track).expect("album");
    rig.scan_store
        .execute_batch_for_tests(&format!(
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
        rig.scan_store.album_for_track(&track).as_deref(),
        Some(album.as_str()),
        "the retagged album keeps its row"
    );
    assert_eq!(
        rig.scan_store
            .query_i64_for_tests(&format!(
                "SELECT COUNT(*) FROM local_albums WHERE id = '{album}' AND title = 'Renamed'"
            ))
            .expect("count reads"),
        1,
        "the kept row takes the new name"
    );
    assert_eq!(
        rig.scan_store
            .query_i64_for_tests("SELECT COUNT(*) FROM local_albums")
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
    use droppedneedle::library::identify::models::{AlbumIdentity, DecisionSource, TrackIdentity};

    let (_scratch, library, music) = bare_library("lib-manual-identity");
    plant(&music, "album/01.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .scan_store
        .execute_batch_for_tests(&format!(
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
    store.save_album_identity(AlbumIdentity {
        local_album_id: album.clone(),
        provider: "musicbrainz".to_owned(),
        release_group_mbid: Some("rg-download".to_owned()),
        release_mbid: Some("rel-download".to_owned()),
        decision_source: DecisionSource::Automatic,
        row_revision: 1,
    });
    store.save_track_identity(TrackIdentity {
        local_track_id: track.clone(),
        provider: "musicbrainz".to_owned(),
        recording_mbid: Some("rec-download".to_owned()),
        release_track_mbid: None,
        decision_source: DecisionSource::Automatic,
        row_revision: 1,
    });

    let kept = store.album_identity(&album).expect("album identity");
    assert_eq!(kept.decision_source, DecisionSource::Manual);
    assert_eq!(kept.release_group_mbid.as_deref(), Some("rg-chosen"));
    assert_eq!(kept.release_mbid.as_deref(), Some("rel-chosen"));
    assert_eq!(kept.row_revision, 3);
    let kept = store.track_identity(&track).expect("track identity");
    assert_eq!(kept.decision_source, DecisionSource::LegacyImport);
    assert_eq!(kept.recording_mbid.as_deref(), Some("rec-chosen"));
    assert_eq!(kept.row_revision, 3);

    // A row no person decided stays revisable.
    library
        .scan_store
        .execute_batch_for_tests(
            "UPDATE local_track_external_identities SET decision_source = 'embedded';",
        )
        .expect("source flipped");
    store.save_track_identity(TrackIdentity {
        local_track_id: track.clone(),
        provider: "musicbrainz".to_owned(),
        recording_mbid: Some("rec-download".to_owned()),
        release_track_mbid: None,
        decision_source: DecisionSource::Automatic,
        row_revision: 4,
    });
    let revised = store.track_identity(&track).expect("track identity");
    assert_eq!(revised.recording_mbid.as_deref(), Some("rec-download"));
}

/// An approval that cannot seal its identity fails as a whole: the
/// caller gets the error and the review stays pending.
#[tokio::test]
async fn failed_approval_leaves_the_review_pending() {
    use droppedneedle::library::service::ServiceError;

    let (_scratch, library, music) = bare_library("lib-approve-fail");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identify_tick().await, 1);
    file_edition_review(&library.identify_store, &album);
    let review = library.pending_reviews(&album);
    assert_eq!(review.len(), 1);
    library
        .scan_store
        .execute_batch_for_tests(
            "CREATE TRIGGER refuse_identity BEFORE INSERT \
             ON local_album_external_identities BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .expect("trigger installs");

    let refused = library.approve_review(&review[0].id, "curator", &format!("rg-1:{REL_1}"));
    assert!(
        matches!(refused, Err(ServiceError::Internal { .. })),
        "a store failure is an error, not an approval"
    );
    assert_eq!(
        library.pending_reviews(&album).len(),
        1,
        "review stays pending"
    );
}

/// A publish journal for a root that is gone never stops boot: recovery
/// finishes, leaves the bundle as it is for a later pass (its root may
/// come back), and reports it held.
#[tokio::test]
async fn journal_on_a_removed_root_waits() {
    let (scratch, first, music) = bare_library("lib-journal-gone");
    plant(&music, "album/01.flac", "flac_full_01.flac");
    add_music_root(&first, &music).await;
    first
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, source_root, \
             source_rel, dest_root, dest_rel, staged, staged_sha256, state) \
             VALUES ('j-1', 'bundle-1', 'audio', 'removed', 'a/01.flac', 'removed', \
             'b/01.flac', '/nowhere/.staged', 'abc', 'cleanup_pending');",
        )
        .expect("journal seed");
    drop(first);

    let library = open_library(&scratch);
    let recovery = library.run_recovery().await;

    assert_eq!(recovery.publish_recoveries.len(), 1);
    assert!(recovery.publish_recoveries[0].1.starts_with("Deferred"));
    assert_eq!(
        library.held_publish_bundles().expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
    assert_eq!(
        library
            .scan_store
            .query_i64_for_tests(
                "SELECT COUNT(*) FROM library_publish_journal \
                 WHERE id = 'j-1' AND state = 'cleanup_pending'"
            )
            .expect("state reads"),
        1,
        "the bundle is left as it was"
    );
    library.publish_tick().expect("maintenance retries");
    assert_eq!(
        library.held_publish_bundles().expect("held reads"),
        vec!["bundle-1".to_owned()]
    );
}

/// Identification state is durable: a job the scan queued survives a
/// restart, and a curator's approval stays a manual identity after the
/// next one.
#[tokio::test]
async fn identification_survives_restarts() {
    use droppedneedle::library::identify::models::DecisionSource;

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
        .expect("track indexed");
    let album = rig.scan_store.album_for_track(&track).expect("album");

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
    rig.identified(1).await;
    rig.choose_first_edition(&album).await;

    let rig = rig.restart().await;
    let identity = rig
        .identify_store
        .album_identity(&album)
        .expect("identity survives");
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.release_mbid.as_deref(), Some(REL_1));
    rig.stop().await;
}

/// Identify the album, then have a person choose the first of two tied
/// editions (the automatic pass only keeps a best guess).
async fn approve_first_candidate(library: &LibrarySetup, album: &str) {
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, album));
    assert_eq!(library.identify_tick().await, 1);
    choose_first_edition(library, album).await;
}

/// A person chooses `rel-1` as the album's edition.
async fn choose_first_edition(library: &LibrarySetup, album: &str) {
    droppedneedle::library::operations::service::Operations::new(library)
        .choose_edition(album, REL_1, Some("curator"))
        .await
        .expect("edition chosen");
}

/// Preview and apply one managed write; returns the bundle id.
fn publish_one(
    library: &LibrarySetup,
    kind: droppedneedle::library::publish::planner::PlanKind,
    album: &str,
    rel_path: &str,
    dest_rel: Option<&str>,
    updates: &[(&str, &str)],
) -> String {
    use droppedneedle::library::manage::PreviewItemInput;
    let sealed = library
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
        .apply_preview(&sealed.token)
        .expect("apply publishes")
        .bundle_id
}

/// Values a new edit could never carry (a `3/12` track number, a
/// free-text date) come back exactly when a retag is undone.
#[tokio::test]
async fn undo_restores_values_no_new_edit_could_write() {
    use droppedneedle::library::publish::planner::PlanKind;
    use droppedneedle::library::tags::save::{TagEdit, save_tags};
    use droppedneedle::library::tags::{TagField, read_fields};

    let (_scratch, library, music) = bare_library("lib-undo-raw");
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
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    approve_first_candidate(&library, &album).await;
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

    library.undo_bundle(&bundle).expect("undo publishes");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// Baseline restore takes a file back to how it was before its first
/// managed write, removing fields a later write added: organize first
/// (no tags written), retag second, restore last.
#[tokio::test]
async fn baseline_restore_removes_fields_added_later() {
    use droppedneedle::library::publish::planner::PlanKind;
    use droppedneedle::library::tags::{TagField, read_fields};

    let (_scratch, library, music) = bare_library("lib-baseline-added");
    let file = plant(&music, "album/01.flac", "flac_full_02.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "album/01.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    approve_first_candidate(&library, &album).await;
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
        .baseline_restore(std::slice::from_ref(&track))
        .expect("restore publishes");
    assert!(file.is_file(), "the file is back where it started");
    assert_eq!(read_fields(&file).expect("fields read"), before);
}

/// A release document answers to its merged ids through their own rows,
/// and a refetch by the canonical id (no redirect, so no merged ids)
/// replaces every row and still answers to them.
#[test]
fn release_documents_answer_to_merged_ids() {
    let (_scratch, library, _music) = bare_library("lib-merged-release");
    let store = &library.identify_store;
    let mut release = Release {
        id: "rel-new".to_owned(),
        title: "Merged".to_owned(),
        old_ids: vec!["REL-OLD".to_owned()],
        ..Release::default()
    };
    store.save_release(&release);
    release.title = "Refetched".to_owned();
    release.old_ids.clear();
    store.save_release(&release);
    for id in ["rel-new", "rel-old"] {
        let found = store.release(id, None).expect("document found");
        assert_eq!(found.id, "rel-new");
        assert_eq!(found.title, "Refetched");
    }
    assert!(store.release("rel-other", None).is_none());
}

/// Contribution journey through the real routes and the SQLite store:
/// draft, duplicate check, seeded editor, the release editor coming back
/// on the v1 callback path, verification, and the album linked.
#[tokio::test]
async fn library_contribution_seed_callback_verify_links_album() {
    use droppedneedle::library::contrib::models::{
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
    tagged_from(&store.album_facts(album_id).expect("album facts"))
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
    let conn = droppedneedle::db::open_connection(db).expect("db opens");
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

/// Automatic edition undone to review, then an explicit re-identification:
/// controls, candidate review, the administrator's choice, and the
/// edition finder, all through the HTTP routes.
#[tokio::test]
async fn library_journey_undo_edition_then_reidentify() {
    use droppedneedle::library::identify::sources::{Edition, EditionPage};
    use droppedneedle::library::operations::decisions::live_automatic_edition_undo;

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
        let facts = rig.identify_store.album_facts(&album).expect("facts");
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
        let conn =
            droppedneedle::db::open_connection(&app.config.library_db_path).expect("db opens");
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
    rig.identified(1).await;
    let sealed = rig.identify_store.album_identity(&album).expect("sealed");
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
    assert!(rig.identify_store.album_identity(&album).is_none());
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
    let (status, _) = app.call("POST", &undo, &headers, Some(fresh)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an undo is used once");

    // Start a re-identification; the same key returns the same job.
    let start = format!("/api/v3/library/albums/{album}/reidentify");
    let key = json!({"idempotency_key": "again-1"});
    let (status, job) = app.call("POST", &start, &headers, Some(key.clone())).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["state"], json!("queued"));
    assert_eq!(job["kind"], json!("explicit_reidentification"));
    let job_id = job["id"].as_str().expect("job id").to_owned();
    let (_, again) = app.call("POST", &start, &headers, Some(key)).await;
    assert_eq!(again["id"], json!(job_id));

    // Stop it while queued, then resume: it starts over.
    let operation = format!("/api/v3/library/operations/{job_id}");
    let (status, stopped) = app
        .call(
            "POST",
            &format!("{operation}/stop"),
            &headers,
            Some(json!({"expected_row_revision": job["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["state"], json!("stopped"));
    let (status, body) = app
        .call(
            "POST",
            &format!("{operation}/resume"),
            &headers,
            Some(json!({"expected_row_revision": job["row_revision"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale revision: {body}");
    let (_, resumed) = app
        .call(
            "POST",
            &format!("{operation}/resume"),
            &headers,
            Some(json!({"expected_row_revision": stopped["row_revision"]})),
        )
        .await;
    assert_eq!(resumed["state"], json!("queued"));

    // The worker scores the candidates and waits for a choice.
    let ready = app
        .until("the worker scores the candidates", || async {
            let (status, job) = app.call("GET", &operation, &headers, None).await;
            assert_eq!(status, StatusCode::OK, "{job}");
            (!matches!(job["state"].as_str(), Some("queued" | "running"))).then_some(job)
        })
        .await;
    assert_eq!(ready["state"], json!("ready"), "{ready}");
    let candidates = ready["reidentification_candidates"]
        .as_array()
        .expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["automatic_safe"], json!(true));
    assert_eq!(
        candidates[0]["evidence"]["track_evidence"][0]["classification"],
        json!("supported")
    );
    assert!(
        rig.identify_store.album_identity(&album).is_none(),
        "nothing seals before the choice"
    );

    // The edition finder marks nothing current yet.
    scripted.set_editions(EditionPage {
        items: vec![Edition {
            release_mbid: release.id.clone(),
            release_group_mbid: release.release_group_id.clone(),
            title: release.title.clone(),
            score: 100,
            ..Edition::default()
        }],
        total: 1,
        offset: 0,
    });
    let finder =
        format!("/api/v3/library/albums/{album}/reidentification/releases?title=OK%20Computer");
    let (status, found) = app.call("GET", &finder, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["items"][0]["is_current_release"], json!(false));

    // The choice seals a manual identity and settles the review.
    let (status, done) = app
        .call(
            "POST",
            &format!("{operation}/candidate"),
            &headers,
            Some(json!({
                "expected_row_revision": ready["row_revision"],
                "candidate_key": candidates[0]["candidate_key"],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["state"], json!("succeeded"));
    assert_eq!(done["terminal_code"], json!("IDENTIFIED"));
    assert_eq!(
        done["selected_reidentification_candidate_key"],
        candidates[0]["candidate_key"]
    );
    let manual = rig.identify_store.album_identity(&album).expect("sealed");
    assert_eq!(manual.release_mbid.as_deref(), Some(release.id.as_str()));
    assert_eq!(manual.decision_source.as_str(), "manual");
    let (_, body) = app.call("GET", &reviews, &headers, None).await;
    assert_eq!(body["reviews"], json!([]), "the review is settled");
    let (_, found) = app.call("GET", &finder, &headers, None).await;
    assert_eq!(found["items"][0]["is_current_release"], json!(true));
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
        let conn =
            droppedneedle::db::open_connection(&app.config.library_db_path).expect("db opens");
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
    assert_eq!(body["state"], "idle", "{body}");
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
    use droppedneedle::library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-remove");
    let first = plant(&music, "Band/LP/01.flac", "flac_full_01.flac");
    let second = plant(&music, "Band/LP/02.flac", "flac_full_01.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .expect("track indexed");
    let album: String = droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens")
        .query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [&track],
            |row| row.get(0),
        )
        .expect("album id");
    library
        .scan_store
        .execute_batch_for_tests(
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
        .remove_track(&track, true, "admin")
        .expect("track removed");
    assert_eq!(removed.track_ids, vec![track.clone()]);
    assert!(!first.exists(), "the file left the album folder");
    let bin = music.join(".recycle");
    assert_eq!(std::fs::read_dir(&bin).expect("bin exists").count(), 1);

    let removed = Mutations::new(&library)
        .remove_album(&album, true, "admin")
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
            .query_i64_for_tests("SELECT COUNT(*) FROM local_tracks WHERE availability = 'missing'")
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

/// "Put back" never replaces a file that now sits where the removed one
/// was: it is refused, the newer file is untouched and the removed file
/// stays in the recycle bin, where a later "Put back" still finds it.
#[tokio::test]
async fn put_back_refuses_an_occupied_path() {
    use droppedneedle::library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-put-back");
    let original = plant(&music, "Band/LP/01.flac", "flac_full_01.flac");
    let removed_bytes = std::fs::read(&original).expect("removed file reads");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "Band/LP/01.flac")
        .expect("track indexed");
    let album: String = droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db"))
        .expect("db opens")
        .query_row(
            "SELECT local_album_id FROM local_tracks WHERE id = ?1",
            [&track],
            |row| row.get(0),
        )
        .expect("album id");
    library
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");

    let (action, _) = Mutations::new(&library)
        .recycle_album_files(&album, std::slice::from_ref(&track), "admin")
        .expect("file recycled");
    let recycled = walk_files(&music.join(".recycle"));
    assert_eq!(recycled.len(), 1, "one file in the bin");
    plant(&music, "Band/LP/01.flac", "flac_full_02.flac");
    let newer_bytes = std::fs::read(&original).expect("newer file reads");
    assert_ne!(newer_bytes, removed_bytes);

    let refused = Mutations::new(&library).restore_recycled(&album, &action, "admin");
    assert!(
        matches!(
            &refused,
            Err(MutationError::Conflict(reason)) if reason.code == "RESTORE_PATH_OCCUPIED"
        ),
        "{refused:?}"
    );
    assert!(
        !Mutations::new(&library).put_back(&[(original.clone(), recycled[0].clone())], "admin"),
        "the upgrade put-back refuses too"
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
    assert!(
        Mutations::new(&library)
            .restore_recycled(&album, &action, "admin")
            .expect("put back once the path is free"),
    );
    drain_scans(&library).await;
    assert_eq!(
        std::fs::read(&original).expect("restored file reads"),
        removed_bytes
    );
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
    rig.identified(1).await;
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
    let unchanged = rig.identify_store.album_identity(&album).expect("row");
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
    rig.identified(1).await;
    let kept = rig.identify_store.album_identity(&album).expect("row");
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
    rig.identified(1).await;
    let back = rig.identify_store.album_identity(&album).expect("row");
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

/// Catalog corrections end to end: split a track off, find it still there
/// after a rescan, reset it back to the original album, merge a copy in
/// with its chosen edition following it, and merge a duplicate artist.
/// Applies refuse a token that does not match, and a replayed key returns
/// the first result.
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
    let a1 = store.track_at("music", "a/01.flac").expect("a/01");
    let a2 = store.track_at("music", "a/02.flac").expect("a/02");
    let b1 = store.track_at("music", "b/01.flac").expect("b/01");
    let album_a = store.album_for_track(&a1).expect("album a");
    let album_b = store.album_for_track(&b1).expect("album b");
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
        store.album_for_track(&a2).as_deref(),
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
        store.album_for_track(&a2).as_deref(),
        Some(album_new.as_str())
    );
    let (status, replay) = app.call("POST", &split, &headers, Some(apply)).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["target_album_id"], json!(album_new));

    // A rescan keeps the manual grouping.
    scan().await;
    assert_eq!(
        store.album_for_track(&a2).as_deref(),
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
        store.album_for_track(&a2).as_deref(),
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
        store.album_for_track(&b1).as_deref(),
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
    rig.stop().await;
}

/// Run one Library Management worker step off the async runtime.
async fn management_step(library: &LibrarySetup) -> bool {
    let management = droppedneedle::library::management::service::Management::new(library);
    tokio::task::spawn_blocking(move || management.run_next("test-worker"))
        .await
        .expect("worker joins")
        .expect("worker step runs")
}

/// Make a ready preview and apply it; returns the job id once the worker
/// wrote it.
async fn management_run(
    library: &LibrarySetup,
    request: droppedneedle::library::management::service::PreviewRequest,
) -> String {
    use droppedneedle::library::management::service::{ApplyRequest, Management};
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
    use droppedneedle::library::management::models::{ItemFilter, Selection, SelectionKind};
    use droppedneedle::library::management::service::{ApplyRequest, Management, PreviewRequest};
    use droppedneedle::library::tags::save::{TagEdit, save_tags};
    use droppedneedle::library::tags::{TagField, read_fields};
    use droppedneedle::runtime_config::sections::NamingScript;
    use droppedneedle::settings::management as lm;

    let (_scratch, library, music) = bare_library("lib-management-jobs");
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
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identify_tick().await, 1);
    library
        .scan_store
        .execute_batch_for_tests(
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
    let settings = library.config.save(settings).expect("settings saved");

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
            expected_policy_revision: library.live_registry().policy_revision().to_owned(),
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
    droppedneedle::library::operations::service::Operations::new(&library)
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
            .all(|item| item.work_state == "succeeded")
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
    rig.identified(1).await;

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
    assert!(rig.identify_store.album_identity(&album).is_none());
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
    let restored = rig.identify_store.album_identity(&album).expect("restored");
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
        use droppedneedle::runtime_config::sections::LibraryManagementProfile;
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
        let conn =
            droppedneedle::db::open_connection(&app.config.library_db_path).expect("db opens");
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
    let accepted = rig.identify_store.album_identity(&album).expect("accepted");
    assert_eq!(accepted.release_mbid.as_deref(), Some(release.id.as_str()));
    // A confident acceptance is applied as confirmed: no flag remains.
    let match_state = || -> Option<String> {
        use rusqlite::OptionalExtension as _;
        droppedneedle::db::open_connection(&app.config.library_db_path)
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
    assert!(rig.identify_store.album_identity(&album).is_none());
    // The undo puts back the match state the acceptance replaced.
    assert_eq!(match_state().as_deref(), Some("unmatched:NO_CANDIDATE"));
    rig.stop().await;
}

/// Apply a ready preview from its handle and run the worker once.
async fn management_apply(
    library: &LibrarySetup,
    handle: &droppedneedle::library::management::models::PreviewHandle,
) -> droppedneedle::library::management::models::JobDetail {
    use droppedneedle::library::management::service::{ApplyRequest, Management};
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
                    .map(|item| (item.work_state.clone(), item.failure.clone()))
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
    use droppedneedle::library::management::activation::{
        ActivationConfirm, ActivationProof, ActivationRequest,
    };
    use droppedneedle::library::management::models::{Selection, SelectionKind, TagEditMode};
    use droppedneedle::library::management::restore::{PurgeRequest, RestoreRequest};
    use droppedneedle::library::management::service::Management;
    use droppedneedle::library::management::tag_edit::TagEditRequest;
    use droppedneedle::library::tags::{TagField, read_fields};
    use droppedneedle::settings::management as lm;

    let (scratch, library, music) = bare_library("lib-management-tools");
    let file = plant(&music, "incoming/tool track.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "incoming/tool track.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identify_tick().await, 1);
    library
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    droppedneedle::library::operations::service::Operations::new(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    library
        .config
        .save(lm::initial_settings(""))
        .expect("settings saved");
    // The settings page reads them first, which stores them normalized.
    lm::service::LibraryManagementService::new(
        library.config.clone(),
        library.ids.clone(),
        Arc::new(lm::StructuralCompiler),
    )
    .get_settings()
    .expect("settings normalized");
    let original = read_fields(&file).expect("original tags");
    let title =
        |path: &std::path::Path| read_fields(path).expect("tags read")[&TagField::Title].clone();
    let management = Management::new(&library);
    let policy = library.live_registry().policy_revision().to_owned();
    let revision = || {
        lm::settings_revision(
            &library
                .config
                .get::<droppedneedle::runtime_config::sections::LibraryManagement>()
                .expect("settings read"),
        )
    };
    let overrides = |_: &LibrarySetup| -> i64 {
        droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db"))
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
        use droppedneedle::library::management::enrich::{Cover, LyricsFound, ScriptedEnrichment};
        use droppedneedle::library::tags::save::read_document;
        use droppedneedle::runtime_config::sections::{LibraryManagement, ReplayGainMode};
        let db = || {
            droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db")
        };
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
        assert!(
            library
                .enrichment
                .set(Arc::new(ScriptedEnrichment {
                    listenbrainz: None,
                    lyrics: Some(LyricsFound {
                        plain: Some("Plain words".to_owned()),
                        synced: Some(synced.clone()),
                    }),
                    cover: Some(Cover {
                        bytes: cover.clone(),
                        mime: "image/png".to_owned(),
                    }),
                }))
                .is_ok()
        );
        droppedneedle::library::tags::save_tags(
            &file,
            &[droppedneedle::library::tags::TagEdit::native(
                "vorbis:MOOD",
                vec!["Calm".to_owned()],
            )],
        )
        .expect("custom tag planted");
        rescan(&library).await;
        let before = read_document(&file).expect("tags before");
        let stored: LibraryManagement = library.config.get().expect("settings");
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
        library.config.save(enriched).expect("profile saved");
        let normalize = || {
            lm::service::LibraryManagementService::new(
                library.config.clone(),
                library.ids.clone(),
                Arc::new(lm::StructuralCompiler),
            )
            .get_settings()
            .expect("settings normalized");
        };
        normalize();
        let run = management
            .create_preview(
                droppedneedle::library::management::service::PreviewRequest {
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
        library.config.save(stored).expect("profile back");
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
        expected_catalog_revision: impact.catalog_revision,
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
        .config
        .get::<droppedneedle::runtime_config::sections::LibraryManagement>()
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
            droppedneedle::runtime_config::sections::LibraryManagementRootAssignment {
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
        droppedneedle::library::management::service::ApplyRequest {
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

    // The album is then managed without asking: moved by the profile.
    library.queue_automatic_management(&album);
    assert!(management.automatic_next().expect("automatic step"));
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
    library.queue_automatic_management(&album);
    assert!(management.automatic_next().expect("automatic step"));
    let (again, _) = management.history(Default::default()).expect("history");
    assert_eq!(again.len(), history.len());

    // A new match revision (a new edition) is managed again.
    let db =
        || droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db");
    db().execute(
        "UPDATE local_album_external_identities SET row_revision = row_revision + 1 \
         WHERE local_album_id = ?1",
        [&album],
    )
    .expect("identity bumped");
    library.queue_automatic_management(&album);
    assert!(management.automatic_next().expect("automatic step"));
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
        .expect("second copy indexed");
    let second_album = library
        .scan_store
        .album_for_track(&second_track)
        .expect("second album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &second_album));
    library.identify_tick().await;
    droppedneedle::library::operations::service::Operations::new(&library)
        .confirm_match(&second_album, "admin")
        .expect("second match confirmed");
    let settings_now = revision();
    let clash = management
        .create_preview(
            droppedneedle::library::management::service::PreviewRequest {
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
            item.eligibility == droppedneedle::library::management::models::Eligibility::Blocked
        })
        .expect("the copy is blocked by the clash");
    let source = management.detail(&clash.job_id).expect("clash detail");
    let replace_request = |key: &str| {
        droppedneedle::library::management::duplicates::DuplicateRequest {
        intent: droppedneedle::library::management::models::DuplicateIntent {
            source_job_id: clash.job_id.clone(),
            source_plan_item_ordinal: blocked.ordinal,
            collision_kind: "same_path_different_content".to_owned(),
            existing_root_id: "music".to_owned(),
            existing_relative_path: managed_rel.clone(),
            existing_local_track_id: None,
            action: droppedneedle::library::management::models::DuplicateAction::RecycleExistingKeepIncoming,
            alternate_relative_path: None,
        },
        expected_source_row_revision: source.job.row_revision,
        expected_settings_revision: settings_now.clone(),
        expected_policy_revision: policy.clone(),
        idempotency_key: key.to_owned(),
    }
    };
    let apply_request = |handle: &droppedneedle::library::management::models::PreviewHandle| {
        droppedneedle::library::management::service::ApplyRequest {
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
        droppedneedle::library::operations::service::Operations::new(&library)
            .control(
                job_id,
                droppedneedle::library::operations::models::Control::Stop,
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
    library.publish_tick().expect("maintenance runs");
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
    library.arm_publish_crash(Some(
        droppedneedle::library::publish::CrashPoint::AfterFirstRename,
    ));
    management
        .apply(&resolve.job_id, apply_request(&resolve))
        .expect("resolution queued");
    management_step(&library).await;
    library.arm_publish_crash(None);
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
    library.publish_tick().expect("recovery runs");
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
    use droppedneedle::library::management::models::{Selection, SelectionKind};
    use droppedneedle::library::management::service::{Management, PreviewRequest};
    use droppedneedle::library::tags::{TagField, read_fields};
    use droppedneedle::settings::management as lm;

    let (scratch, library, music) = bare_library("lib-management-custom");
    let file = plant(&music, "custom/sealed track.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let track = library
        .scan_store
        .track_at("music", "custom/sealed track.flac")
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identify_tick().await, 1);
    library
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    droppedneedle::library::operations::service::Operations::new(&library)
        .confirm_match(&album, "admin")
        .expect("match confirmed");
    library
        .config
        .save(lm::initial_settings(""))
        .expect("settings saved");
    lm::service::LibraryManagementService::new(
        library.config.clone(),
        library.ids.clone(),
        Arc::new(lm::StructuralCompiler),
    )
    .get_settings()
    .expect("settings normalized");
    // Seal the album as a custom edition: its release is let go, and the
    // manifest records the files as they are.
    let db =
        || droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db");
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
            .config
            .get::<droppedneedle::runtime_config::sections::LibraryManagement>()
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
        expected_policy_revision: library.live_registry().policy_revision().to_owned(),
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
    use droppedneedle::library::management::enrich::{Cover, ScriptedEnrichment};
    use droppedneedle::library::management::models::{Selection, SelectionKind};
    use droppedneedle::library::management::service::{Management, PreviewRequest};
    use droppedneedle::library::tags::save::read_document;
    use droppedneedle::library::tags::{TagEdit, TagField};
    use droppedneedle::runtime_config::sections::LibraryManagement;
    use droppedneedle::settings::management as lm;

    let (scratch, library, music) = bare_library("lib-management-artwork");
    let file = plant(&music, "art/cover track.m4a", "management_full.m4a");
    droppedneedle::library::tags::save_tags(
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
        .expect("track indexed");
    let album = library.scan_store.album_for_track(&track).expect("album");
    library
        .test_providers
        .as_ref()
        .expect("scripted providers")
        .set_recall(ambiguous_recall(&library.identify_store, &album));
    assert_eq!(library.identify_tick().await, 1);
    library
        .scan_store
        .execute_batch_for_tests(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('admin', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
        )
        .expect("admin row");
    droppedneedle::library::operations::service::Operations::new(&library)
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
    library.config.save(settings).expect("settings saved");
    let original = read_document(&file).expect("original tags");
    lm::service::LibraryManagementService::new(
        library.config.clone(),
        library.ids.clone(),
        Arc::new(lm::StructuralCompiler),
    )
    .get_settings()
    .expect("settings normalized");
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
    assert!(
        library
            .enrichment
            .set(Arc::new(ScriptedEnrichment {
                listenbrainz: None,
                lyrics: None,
                cover: Some(Cover {
                    bytes: cover,
                    mime: "image/png".to_owned(),
                }),
            }))
            .is_ok()
    );
    let db =
        || droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db");
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
                .config
                .get::<LibraryManagement>()
                .expect("settings read"),
        ),
        expected_policy_revision: library.live_registry().policy_revision().to_owned(),
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
    assert_eq!(
        after.native.get("mp4:\u{a9}wrt"),
        Some(&vec!["Ada Composer".to_owned()]),
        "composer kept"
    );
    assert!(
        after
            .native
            .keys()
            .any(|key| key.eq_ignore_ascii_case("mp4:----:MusicBrainz Work Id")),
        "work id kept"
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
            droppedneedle::library::management::restore::RestoreRequest {
                selection: Selection {
                    kind: SelectionKind::Albums,
                    ids: vec![album.clone()],
                    catalog_filter: None,
                },
                expected_settings_revision: lm::settings_revision(
                    &library
                        .config
                        .get::<LibraryManagement>()
                        .expect("settings read"),
                ),
                expected_policy_revision: library.live_registry().policy_revision().to_owned(),
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
    scratch: ScratchDir,
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
        configure: impl Fn(&mut droppedneedle::runtime_config::sections::LibraryManagementProfile),
    ) -> Self {
        use droppedneedle::settings::management as lm;
        let (scratch, library, music) = bare_library(tag);
        std::fs::create_dir_all(&music).expect("music folder");
        add_music_root(&library, &music).await;
        library
            .scan_store
            .execute_batch_for_tests(
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
        library.config.save(settings).expect("settings saved");
        lm::service::LibraryManagementService::new(
            library.config.clone(),
            library.ids.clone(),
            Arc::new(lm::StructuralCompiler),
        )
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
        droppedneedle::db::open_connection(&self.scratch.to_path_buf().join("app.db"))
            .expect("db opens")
    }

    fn management(&self) -> droppedneedle::library::management::service::Management {
        droppedneedle::library::management::service::Management::new(&self.library)
    }

    fn settings_revision(&self) -> String {
        droppedneedle::settings::management::settings_revision(
            &self
                .library
                .config
                .get::<droppedneedle::runtime_config::sections::LibraryManagement>()
                .expect("settings read"),
        )
    }

    /// Plant `tracks` copies of the fixture as one album in `folder`,
    /// scan, identify it and confirm the match. Returns the album and its
    /// tracks in order.
    async fn add_album(&self, folder: &str, tracks: usize) -> (String, Vec<String>) {
        use droppedneedle::library::tags::TagField;
        use droppedneedle::library::tags::save::{TagEdit, save_tags};
        let nth = self.albums.replace(self.albums.get() + 1);
        let mut relative = Vec::new();
        for number in 1..=tracks {
            let rel = format!("{folder}/{number:02}.flac");
            let file = plant(&self.music, &rel, "management_full.flac");
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
        rescan(&self.library).await;
        let ids: Vec<String> = relative
            .iter()
            .map(|rel| {
                self.library
                    .scan_store
                    .track_at("music", rel)
                    .expect("track indexed")
            })
            .collect();
        let album = self
            .library
            .scan_store
            .album_for_track(&ids[0])
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
        self.library
            .test_providers
            .as_ref()
            .expect("scripted providers")
            .set_recall(recall);
        assert_eq!(self.library.identify_tick().await, 1);
        droppedneedle::library::operations::service::Operations::new(&self.library)
            .confirm_match(&album, "admin")
            .expect("match confirmed");
        (album, ids)
    }

    /// Turn automatic management on for the folder: a dry run, confirmed.
    async fn activate(&self) {
        use droppedneedle::library::management::activation::{
            ActivationConfirm, ActivationProof, ActivationRequest,
        };
        use droppedneedle::runtime_config::sections::{
            LibraryManagement, LibraryManagementRootAssignment,
        };
        let management = self.management();
        // Identification queued the albums while nothing was automatic
        // yet; the worker looks and drops them.
        while management.automatic_next().expect("automatic step") {}
        let mut proposed = self
            .library
            .config
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
                        droppedneedle::settings::management::PICARD_ORGANIZER_PROFILE_ID.to_owned(),
                    ),
                    enabled: true,
                    automatic_scan_discovered: true,
                    ..Default::default()
                }),
        }
        let dry_run = management
            .create_activation(
                ActivationRequest {
                    root_id: "music".to_owned(),
                    settings: proposed.clone(),
                    expected_settings_revision: self.settings_revision(),
                    expected_policy_revision: self
                        .library
                        .live_registry()
                        .policy_revision()
                        .to_owned(),
                    idempotency_key: Some("dry-run".to_owned()),
                },
                "admin",
            )
            .expect("dry run");
        assert!(
            management_step(&self.library).await,
            "dry run planning runs"
        );
        management
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

    /// Have automatic management look at the album now, and run whatever
    /// it starts to the end.
    async fn look(&self, album: &str) {
        self.library.queue_automatic_management(album);
        self.worker_looks().await;
    }

    /// Let the worker look at every waiting album and finish its runs.
    async fn worker_looks(&self) {
        let management = self.management();
        while management.automatic_next().expect("automatic step") {}
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
    use droppedneedle::library::publish::record::v2_path_revision;
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
    async fn edit_outside(&self, track: &str, edit: droppedneedle::library::tags::TagEdit) {
        droppedneedle::library::tags::save_tags(&self.path(track), &[edit])
            .expect("retagged outside the app");
        rescan(&self.library).await;
    }

    /// A person's preview with the folder's profile, planned.
    async fn preview(
        &self,
        kind: droppedneedle::library::management::models::SelectionKind,
        ids: &[&str],
        key: &str,
    ) -> (
        droppedneedle::library::management::models::PreviewHandle,
        Vec<droppedneedle::library::management::models::PlannedItem>,
    ) {
        use droppedneedle::library::management::models::Selection;
        use droppedneedle::library::management::service::PreviewRequest;
        let management = self.management();
        let handle = management
            .create_preview(
                PreviewRequest {
                    selection: Selection {
                        kind,
                        ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                        catalog_filter: None,
                    },
                    profile_id: droppedneedle::settings::management::PICARD_ORGANIZER_PROFILE_ID
                        .to_owned(),
                    expected_settings_revision: self.settings_revision(),
                    expected_policy_revision: self
                        .library
                        .live_registry()
                        .policy_revision()
                        .to_owned(),
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

fn title_edit(value: &str) -> droppedneedle::library::tags::TagEdit {
    droppedneedle::library::tags::TagEdit::verbatim(
        droppedneedle::library::tags::TagField::Title,
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
    use droppedneedle::library::management::models::{Eligibility, SelectionKind};
    use droppedneedle::library::tags::{TagField, read_fields};

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
    use droppedneedle::library::management::models::TagEditMode;
    use droppedneedle::library::management::tag_edit::TagEditRequest;
    use droppedneedle::library::tags::TagEdit;
    use droppedneedle::library::tags::save::read_document;

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
    droppedneedle::library::operations::service::Operations::new(&managed.library)
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
                    .live_registry()
                    .policy_revision()
                    .to_owned(),
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
    use droppedneedle::library::publish::record::v2_path_revision;

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
    use droppedneedle::library::management::models::SelectionKind;

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
            "SELECT json_extract(w.result_json, '$.bundle_id') FROM library_operation_work w \
             JOIN library_management_job_snapshots s ON s.job_id = w.job_id \
             WHERE s.origin = 'scan_discovered' \
             AND json_extract(w.result_json, '$.bundle_id') IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("the run's bundle");

    managed.library.undo_bundle(&bundle).expect("bundle undone");
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
    use droppedneedle::library::management::enrich::{Cover, ScriptedEnrichment};
    use droppedneedle::library::tags::save::{PictureSpec, save_tags_with_pictures};

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
        let managed = ManagedLibrary::start(tag, |profile| {
            profile.artwork.embedded_enabled = artwork;
        })
        .await;
        assert!(
            managed
                .library
                .enrichment
                .set(Arc::new(ScriptedEnrichment {
                    listenbrainz: None,
                    lyrics: None,
                    cover: Some(Cover {
                        bytes: png([30, 30, 200]),
                        mime: "image/png".to_owned(),
                    }),
                }))
                .is_ok()
        );
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
    let a2 = store.track_at("music", "a/02.flac").expect("a/02");
    let b1 = store.track_at("music", "b/01.flac").expect("b/01");
    let album_a = store.album_for_track(&a2).expect("album a");
    let album_b = store.album_for_track(&b1).expect("album b");
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
    use droppedneedle::library::mutations::Mutations;
    use droppedneedle::library::tags::{TagField, read_fields};

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

/// Handing every album back at once only stamps them: no file is read in
/// the request. The worker then hands back one album a step, and a
/// restart in between loses none.
#[tokio::test]
async fn handing_every_album_back_takes_one_step_each() {
    use droppedneedle::library::management::automatic::Settle;
    use droppedneedle::library::management::service::Management;
    use droppedneedle::library::mutations::Mutations;

    let (scratch, library, music) = bare_library("lib-takeover-hand-back-all");
    for folder in ["a", "b", "c"] {
        plant(&music, &format!("{folder}/01.flac"), "management_full.flac");
    }
    add_music_root(&library, &music).await;
    let db = droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db");
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
    let library = open_library(&scratch);
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
        .expect("track");
    let album = rig.scan_store.album_for_track(&track).expect("album");
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
    use droppedneedle::library::management::automatic::Settle;
    use droppedneedle::library::management::service::Management;
    use droppedneedle::library::mutations::{MutationError, Mutations};

    let (scratch, library, music) = bare_library("lib-takeover-hand-back-fails");
    let file = plant(&music, "a/01.flac", "management_full.flac");
    add_music_root(&library, &music).await;
    let db = droppedneedle::db::open_connection(&scratch.to_path_buf().join("app.db")).expect("db");
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
    use droppedneedle::library::mutations::Mutations;

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
        use droppedneedle::library::tags::{TagEdit, TagField, save_tags};
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
    use droppedneedle::library::tags::{TagField, read_fields};
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
    use droppedneedle::library::mutations::Mutations;

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
    use droppedneedle::library::publish::planner::PlanKind;

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
            droppedneedle::library::publish::planner::PlanKind::Move,
            album,
            &self.relative(track),
            Some(dest_rel),
            &[],
        );
        assert!(self.path(track).ends_with(dest_rel));
    }

    /// Change the folder's rules for when it manages albums by itself.
    fn set_automatic(&self, scan_discovered: bool, acquisitions: bool) {
        use droppedneedle::runtime_config::sections::LibraryManagement;
        let mut settings = self
            .library
            .config
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
        self.library.config.save(settings).expect("settings saved");
    }

    /// A person's preview with the folder's profile, planned and applied.
    async fn apply(
        &self,
        kind: droppedneedle::library::management::models::SelectionKind,
        ids: &[&str],
        key: &str,
    ) -> String {
        use droppedneedle::library::management::models::Selection;
        use droppedneedle::library::management::service::PreviewRequest;
        management_run(
            &self.library,
            PreviewRequest {
                selection: Selection {
                    kind,
                    ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                    catalog_filter: None,
                },
                profile_id: droppedneedle::settings::management::PICARD_ORGANIZER_PROFILE_ID
                    .to_owned(),
                expected_settings_revision: self.settings_revision(),
                expected_policy_revision: self.library.live_registry().policy_revision().to_owned(),
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
    use droppedneedle::library::manage::PreviewItemInput;
    use droppedneedle::library::publish::planner::PlanKind;
    use droppedneedle::library::tags::{TagField, read_fields};

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
    assert!(managed.library.apply_preview(&sealed.token).is_err());
    assert_eq!(managed.path(track), file);
    assert_eq!(file_sha256(&file), bytes, "the file untouched");
}

/// A person's run of the whole profile over a hand edit writes every
/// managed field from the edition, so it records what it placed: a later
/// change of edition is the app's to write, and nothing marks the album.
#[tokio::test]
async fn a_persons_profile_run_takes_the_files_back() {
    use droppedneedle::library::management::models::SelectionKind;

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
    droppedneedle::library::operations::service::Operations::new(&managed.library)
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
    use droppedneedle::library::tags::save::read_document;
    use droppedneedle::library::tags::{TagEdit, save_tags};

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
    droppedneedle::library::operations::service::Operations::new(&managed.library)
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
/// with no record, beside the row of the file the app wrote. It still
/// counts as changed since the app wrote it: the album is left unmanaged
/// and nothing is written. Files that only went away mark nothing.
#[tokio::test]
async fn a_file_retagged_and_renamed_together_is_still_the_users() {
    use droppedneedle::library::mutations::Mutations;
    use droppedneedle::library::tags::{TagEdit, TagField, save_tags};

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
    while managed.library.identify_tick().await > 0 {}
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
            .expect("track indexed")
    };
    for file in &files {
        let track = new_row(file);
        assert!(!whole_tracks.contains(&track), "a new track row");
        assert_eq!(managed.album_of(&track), whole, "beside the old rows");
        assert_eq!(managed.recorded(&track), None);
    }
    assert_eq!(managed.album_of(&new_row(&retitled)), one);
    let bytes =
        |paths: &[PathBuf]| -> Vec<String> { paths.iter().map(|p| file_sha256(p)).collect() };
    let whole_bytes = bytes(&files);
    let one_bytes = bytes(std::slice::from_ref(&retitled));

    for album in [&whole, &one, &pruned] {
        managed.look(album).await;
    }
    // The whole album's new rows never get track matches here, so the
    // worker waits for them and writes nothing.
    let skipped = managed.management().automatic_skipped(10).expect("skipped");
    assert!(
        skipped
            .iter()
            .any(|album| album.album_id == whole && album.reason.code == "TRACK_NOT_MAPPED"),
        "{skipped:?}"
    );
    assert_eq!(bytes(&files), whole_bytes, "nothing written");
    // Mapped the way the old rows were, as identification would map
    // them: the worker counts the new rows as changed and steps back.
    for (old, file) in whole_tracks.iter().zip(&files) {
        managed
            .db()
            .execute(
                "INSERT INTO local_track_external_identities (local_track_id, provider, \
                 recording_mbid, release_mbid, release_track_mbid, medium_position, \
                 release_track_position, decision_source, selected_at) \
                 SELECT ?2, provider, recording_mbid, release_mbid, release_track_mbid, \
                 medium_position, release_track_position, 'automatic', selected_at \
                 FROM local_track_external_identities WHERE local_track_id = ?1",
                [old, &new_row(file)],
            )
            .expect("track mapped");
    }
    managed.look(&whole).await;
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

    // Handed back: the rows left behind lose their record with it, so the
    // album is not marked again.
    assert!(
        Mutations::new(&managed.library)
            .hand_back_management(&whole, "admin")
            .expect("hand-back answers")
    );
    managed.worker_looks().await;
    assert_eq!(managed.mark(&whole), None);
    rescan(&managed.library).await;
    managed.look(&whole).await;
    assert_eq!(managed.mark(&whole), None, "not marked again");
}

/// An import that brings back a track the app wrote and that went missing
/// marks nothing: the app wrote the new file too.
#[tokio::test]
async fn an_import_in_place_of_a_missing_track_marks_nothing() {
    use droppedneedle::library::import::{DownloadImport, ImportOrigin, ImportSource};

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
        .expect("release stored");
    let source = managed.scratch.to_path_buf().join("incoming/02.flac");
    std::fs::create_dir_all(source.parent().expect("parent")).expect("mkdir");
    std::fs::copy(fixtures_dir().join("management_full.flac"), &source).expect("download planted");
    let imported = managed
        .library
        .import_download(&DownloadImport {
            task_id: "task-1".to_owned(),
            origin: ImportOrigin::Download,
            staging: None,
            release,
            files: vec![ImportSource {
                path: source,
                track: 1,
            }],
            extras: Vec::new(),
        })
        .expect("imported");
    rescan(&managed.library).await;
    managed.look(&album).await;
    managed.look(&imported.album_id).await;
    assert_eq!(managed.mark(&album), None);
    assert_eq!(managed.mark(&imported.album_id), None);
    assert_eq!(managed.count(STEPPED_BACK), 0);
}

/// A file v2 tagged, changed by hand since, then renamed in the app: the
/// rename keeps v2's record, so the worker still leaves the album to the
/// user.
#[tokio::test]
async fn a_rename_in_the_app_keeps_v2s_record() {
    use droppedneedle::library::publish::record::v2_path_revision;

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
        .expect("track indexed");
    let album = rig.scan_store.album_for_track(&track).expect("album");
    let conn = droppedneedle::db::open_connection(&app.config.library_db_path).expect("db opens");
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
