//! Full-scan throughput: how many files per second a first scan of a
//! generated library catalogs, on real SQLite with no network.
//!
//! Ignored, so the normal suite never runs it. The baseline size is
//! 100,000 files:
//!
//! ```text
//! DN_SCAN_FILES=100000 cargo test --release --test library \
//!     full_scan_throughput -- --ignored --nocapture
//! ```
//!
//! The database is opened the way boot opens it: the factory's runtime,
//! with its watch thread, and the checkpoint loop on the loop supervisor,
//! so the WAL hold and its relief act as they do in production.
//!
//! Every file lists its artist and album artist (`ARTISTS`,
//! `ALBUMARTISTS`) with their MusicBrainz ids, so each credit is placed by
//! id. `DN_SCAN_ONE_ARTIST=1` credits every file to one artist, which
//! shows whether placing a credit grows with the size of its artist.
//!
//! The tree and the database live under the temp dir (`TMPDIR`) and are
//! removed when the test ends. The files were just written, so the page
//! cache is warm: the number covers the walk, the tag reads and the
//! catalog writes, not cold disk reads, and not identification.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::library::TestSql as _;
use crate::common::library::library_at;
use crate::common::{ScratchDir, config_store, migrated_db};
use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::jobs::checkpoint;
use droppedneedle_kernel::jobs::{CancellationToken, Supervisor};
use droppedneedle_library::scan::{
    Counter, EffectivePolicy, ScanKind, ScanRequest, ScanState, ScanTrigger,
};

const FILES_ENV: &str = "DN_SCAN_FILES";
const ONE_ARTIST_ENV: &str = "DN_SCAN_ONE_ARTIST";
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
/// own track: eight tracks to an album, four albums to an artist (or all
/// on one artist), with MusicBrainz ids that differ per artist, album and
/// track, and the artist lists that line up with the artist ids.
fn generate(root: &Path, files: usize, one_artist: bool) -> u64 {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let template = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library/flac_full_01.flac"),
    )
    .expect("fixture reads");
    for index in 0..files {
        let album = index / TRACKS_PER_ALBUM;
        let artist = if one_artist {
            0
        } else {
            album / ALBUMS_PER_ARTIST
        };
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
        let path = dir.join(format!("{track:02}.flac"));
        std::fs::write(&path, bytes).expect("track writes");
        let name = vec![format!("Art{artist:06}")];
        let id = vec![format!("a74b1b7f-71a5-4011-9441-{artist:012x}")];
        save_tags(
            &path,
            &[
                TagEdit::verbatim(TagField::Artists, name.clone()),
                TagEdit::verbatim(TagField::AlbumArtists, name),
                TagEdit::verbatim(TagField::MusicBrainzArtistId, id.clone()),
                TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, id),
            ],
        )
        .expect("artist lists written");
    }
    (template.len() * files) as u64
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "scan throughput measurement; run explicitly with --release: cargo test --release --test library full_scan_throughput -- --ignored --nocapture"]
async fn full_scan_throughput() {
    let files = match std::env::var(FILES_ENV) {
        Ok(value) => value.parse().expect("DN_SCAN_FILES is a file count"),
        Err(_) => DEFAULT_FILES,
    };
    let scratch = ScratchDir::new("scan-throughput");
    let music = scratch.join("music");
    let one_artist = std::env::var(ONE_ARTIST_ENV).is_ok_and(|value| value == "1");
    let tree_bytes = generate(&music, files, one_artist);

    migrated_db(&scratch.join("app.db"));
    let runtime = open_runtime(&DbConfig::new(&scratch.join("app.db")))
        .await
        .expect("database runtime opens");
    let supervisor = Supervisor::start(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        CancellationToken::new(),
    )
    .await
    .expect("loop supervisor starts");
    checkpoint::spawn_on(
        &supervisor,
        runtime.checkpoint().clone(),
        checkpoint::default_schedule(),
    )
    .expect("checkpoint loop starts");
    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .settings(config_store(&scratch))
        .supervisor(supervisor.clone())
        .build()
        .await;
    let (library, _) = library_at(&platform);
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let roots = library.core.live_registry();

    let started = Instant::now();
    let requested = library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: roots.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: roots.policy_revision().to_owned(),
        })
        .expect("scan requested");
    while library.scans.supervisor_tick().await && !library.coordinator.current().is_empty() {}
    let elapsed = started.elapsed().as_secs_f64();
    let wal_at_end = std::fs::metadata(scratch.join("app.db-wal")).map_or(0, |meta| meta.len());

    let run = library
        .coordinator
        .snapshot(&requested.run_id)
        .expect("run")
        .0;
    let query = |sql: &str| {
        library
            .scan_store
            .db()
            .query_i64(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    };
    let indexed = query("SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed'");
    let albums = query("SELECT COUNT(DISTINCT local_album_id) FROM local_tracks");
    let database_bytes =
        query("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()");

    let artists = query("SELECT COUNT(DISTINCT local_artist_id) FROM local_track_artists");
    println!("full scan: {files} files, {albums} albums, {artists} artists");
    println!("  wall time     {elapsed:.2} s");
    println!("  throughput    {:.0} files/s", files as f64 / elapsed);
    for (phase, seconds) in &run.phase_timings {
        let phase = phase.scan_phase().as_str();
        println!("  {phase:<13} {seconds:.2} s");
    }
    println!(
        "  temp space    tree {:.0} MB of file data, database {:.0} MB",
        tree_bytes as f64 / 1e6,
        database_bytes as f64 / 1e6
    );
    println!(
        "  WAL file      {:.1} MB at the end",
        wal_at_end as f64 / 1e6
    );

    assert_eq!(run.state.scan_state(), ScanState::Completed);
    assert_eq!(run.counters.get(Counter::Indexed), files as i64);
    assert_eq!(indexed, files as i64);

    supervisor.shutdown(Duration::from_secs(5)).await;
    drop(library);
    runtime.shutdown().await;
}
