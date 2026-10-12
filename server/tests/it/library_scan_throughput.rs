//! Full-scan throughput: how many files per second a first scan of a
//! generated library catalogs, on real SQLite with no network.
//!
//! Ignored, so the normal suite never runs it. The baseline size is
//! 100,000 files:
//!
//! ```text
//! DN_SCAN_FILES=100000 cargo test --release --test it \
//!     full_scan_throughput -- --ignored --nocapture
//! ```
//!
//! The tree and the database live under the temp dir (`TMPDIR`) and are
//! removed when the test ends. The files were just written, so the page
//! cache is warm: the number covers the walk, the tag reads and the
//! catalog writes, not cold disk reads, and not identification.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::common::ScratchDir;
use droppedneedle::library::scan::{
    EffectivePolicy, ScanKind, ScanRequest, ScanState, ScanTrigger, counter_names,
};
use droppedneedle::library::wiring::LibrarySetup;

const FILES_ENV: &str = "DN_SCAN_FILES";
const DEFAULT_FILES: usize = 2_000;
const TRACKS_PER_ALBUM: usize = 8;
const ALBUMS_PER_ARTIST: usize = 4;

/// Overwrite every occurrence of `from` with `to`. The lengths match, so
/// the Vorbis comment framing around the value stays valid.
fn replace(bytes: &mut [u8], from: &str, to: &str) {
    assert_eq!(from.len(), to.len(), "{from} and {to} differ in length");
    let mut found = false;
    let mut at = 0;
    while at + from.len() <= bytes.len() {
        if bytes[at..].starts_with(from.as_bytes()) {
            bytes[at..at + to.len()].copy_from_slice(to.as_bytes());
            found = true;
            at += from.len();
        } else {
            at += 1;
        }
    }
    assert!(found, "the fixture no longer holds {from}");
}

/// Write `files` copies of one tagged FLAC fixture, each retagged as its
/// own track: eight tracks to an album, four albums to an artist, with
/// MusicBrainz ids that differ per artist, album and track.
fn generate(root: &Path, files: usize) -> u64 {
    let template = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library/flac_full_01.flac"),
    )
    .expect("fixture reads");
    for index in 0..files {
        let album = index / TRACKS_PER_ALBUM;
        let artist = album / ALBUMS_PER_ARTIST;
        let track = index % TRACKS_PER_ALBUM + 1;
        let mut bytes = template.clone();
        replace(&mut bytes, "=Airbag", &format!("=Song {track}"));
        replace(&mut bytes, "=Radiohead", &format!("=Art{artist:06}"));
        replace(&mut bytes, "=OK Computer", &format!("=Album{album:06}"));
        replace(&mut bytes, "TRACKNUMBER=1", &format!("TRACKNUMBER={track}"));
        replace(&mut bytes, "airbag-0001", &format!("{index:011}"));
        replace(&mut bytes, "d0b5e4122711", &format!("{artist:012x}"));
        replace(&mut bytes, "22c65f834433", &format!("{album:012x}"));
        replace(&mut bytes, "aaaaaaaaaaaa", &format!("{album:012x}"));
        let dir = root.join(format!("Art{artist:06}/Album{album:06}"));
        if track == 1 {
            std::fs::create_dir_all(&dir).expect("album dir creates");
        }
        std::fs::write(dir.join(format!("{track:02}.flac")), bytes).expect("track writes");
    }
    (template.len() * files) as u64
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "scan throughput measurement; run explicitly with --release: cargo test --release --test it full_scan_throughput -- --ignored --nocapture"]
async fn full_scan_throughput() {
    let files = match std::env::var(FILES_ENV) {
        Ok(value) => value.parse().expect("DN_SCAN_FILES is a file count"),
        Err(_) => DEFAULT_FILES,
    };
    let scratch = ScratchDir::new("scan-throughput");
    let music = scratch.join("music");
    let tree_bytes = generate(&music, files);

    let users = droppedneedle::auth::wiring::AuthSetup::for_tests()
        .expect("test auth builds")
        .users;
    let library = LibrarySetup::for_tests(users, Arc::new(droppedneedle::ids::UuidGenerator))
        .expect("library bundle builds");
    library
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let registry = library.live_registry();

    let started = Instant::now();
    let requested = library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    while library.supervisor_tick().await && !library.coordinator.current().is_empty() {}
    let elapsed = started.elapsed().as_secs_f64();

    let run = library
        .coordinator
        .snapshot(&requested.run_id)
        .expect("run")
        .0;
    let query = |sql: &str| {
        library
            .scan_store
            .query_i64_for_tests(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    };
    let indexed = query("SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed'");
    let albums = query("SELECT COUNT(DISTINCT local_album_id) FROM local_tracks");
    let database_bytes =
        query("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()");

    println!("full scan: {files} files, {albums} albums");
    println!("  wall time     {elapsed:.2} s");
    println!("  throughput    {:.0} files/s", files as f64 / elapsed);
    let mut phases: Vec<_> = run.phase_timings.iter().collect();
    phases.sort_by(|a, b| a.0.cmp(b.0));
    for (phase, seconds) in phases {
        println!("  {phase:<13} {seconds:.2} s");
    }
    println!(
        "  temp space    tree {:.0} MB of file data, database {:.0} MB",
        tree_bytes as f64 / 1e6,
        database_bytes as f64 / 1e6
    );

    assert_eq!(run.state, ScanState::Completed);
    assert_eq!(
        run.counters.get(counter_names::INDEXED).copied(),
        Some(files as i64)
    );
    assert_eq!(indexed, files as i64);
}
