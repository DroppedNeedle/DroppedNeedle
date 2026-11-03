//! Owning a release group: the catalog's pages and Discover, and
//! acquisition's want, search, follow and edition reads, give one answer
//! from one definition. A match taken off or withdrawn is never brought
//! back by the files' tags; a guess the tags contradict owns only the group
//! the tags name; an album with no match owns the group all its files name.

use std::collections::HashSet;

use crate::common::ScratchDir;
use droppedneedle_acquire::edition::{chosen_edition, library_holds};
use droppedneedle_catalog::discover::ownership;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_kernel::db::DbRuntime;

/// An MBID-shaped release group or release id.
fn mbid(kind: u16, n: u16) -> String {
    format!("0a0b0c0d-{kind:04x}-4000-8000-{n:012x}")
}

fn group(n: u16) -> String {
    mbid(1, n)
}

fn release(n: u16) -> String {
    mbid(2, n)
}

/// The SQL for album `id` (titled `id`) with one indexed file per entry of
/// `tags` naming that release group (`None`: no id in the file), its match
/// row `(group, release)` by identification, and its match flag
/// `(state, reason)`.
fn album(
    id: &str,
    row: Option<(&str, &str)>,
    flag: Option<(&str, &str)>,
    tags: &[Option<&str>],
) -> String {
    let mut sql = format!(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, grouping_source, \
         created_at, updated_at) VALUES ('{id}', 'r1', '{id}', '{id}', '{id}', 'Aurora', \
         'aurora', 'a1', 1994, 'automatic', 1000, 1000);"
    );
    for (n, tag) in tags.iter().enumerate() {
        let tag = tag.map_or("NULL".to_owned(), |tag| format!("'{tag}'"));
        sql.push_str(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, \
             album_artist_name, album_artist_name_folded, disc_number, track_number, year, \
             duration_seconds, file_format, availability, ingest_source, imported_at, \
             membership_source, embedded_release_group_mbid) VALUES ('{id}-{n}', '{id}', 'r1', \
             '/m/{id}-{n}.flac', '{id}-{n}.flac', 'h-{id}-{n}', 4000, 1, 's', 'Song {n}', \
             'song {n}', 'Aurora', 'aurora', '{id}', '{id}', 'Aurora', 'aurora', 1, {}, 1994, \
             200.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic', {tag});",
            n + 1
        ));
    }
    if let Some((group, release)) = row {
        sql.push_str(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{id}', 'musicbrainz', '{group}', '{release}', 'automatic', 1000);"
        ));
    }
    if let Some((state, reason)) = flag {
        sql.push_str(&format!(
            "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             candidates_json, updated_at) VALUES ('{id}', '{state}', '{reason}', '[]', 1000);"
        ));
    }
    sql
}

struct World {
    runtime: DbRuntime,
    /// Declared last so the database closes before the directory goes.
    _scratch: ScratchDir,
}

async fn world(albums: String) -> World {
    let sql = "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
               updated_at) VALUES ('a1', 'Aurora', 'aurora', 'group', 1000, 1000);"
        .to_owned()
        + &albums;
    let dir = ScratchDir::new("reads-ownership");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    runtime
        .db()
        .write(
            droppedneedle_kernel::db::Lane::Foreground,
            "ownership seed",
            move |tx| {
                tx.execute_batch(&sql)
                    .map_err(droppedneedle_kernel::db::DbError::from)
            },
        )
        .await
        .expect("seed runs");
    World {
        runtime,
        _scratch: dir,
    }
}

/// What every reader says of one release group.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answers {
    /// The wanted watcher, a taskless request, a follower's auto-download
    /// and search ask this.
    holds: bool,
    /// Catalog pages, Discover and radio mark the album owned.
    owned: bool,
    /// The whole owned set Weekly Mix reads.
    in_owned_set: bool,
    /// Discover's charts and queue link to this album.
    discover_links: Option<String>,
    /// The catalog page opens this album.
    page_opens: Option<String>,
    /// The album status view lists these albums.
    status: Vec<String>,
    /// A download of the group is pinned to this release.
    pinned: Option<String>,
    /// The album page's play and quick play open this album.
    plays: Option<String>,
    /// The Download button and the card overlay fetch this album's files.
    downloads: Option<String>,
    /// Discover's Missing Essentials and its mixes count the group as the
    /// library's.
    in_discover_library: bool,
}

async fn answers(world: &World, group: &str) -> Answers {
    let db = world.runtime.db();
    let catalog = LocalCatalog::new(db.clone());
    let asked = [group.to_owned()];
    let set: HashSet<String> = catalog
        .all_owned_release_groups()
        .await
        .expect("owned set")
        .into_iter()
        .collect();
    Answers {
        holds: library_holds(db, group).await.expect("holds"),
        owned: catalog
            .owned_albums(&asked)
            .await
            .expect("owned")
            .contains(group),
        in_owned_set: set.contains(group),
        discover_links: ownership::owned_albums(db, &[group])
            .await
            .expect("discover ownership")
            .remove(group),
        page_opens: catalog
            .album(group)
            .await
            .expect("page")
            .map(|album| album.title),
        status: droppedneedle_catalog::local::store::LocalCatalog::new(db.clone())
            .status_albums(group)
            .await
            .expect("status"),
        pinned: chosen_edition(db, group)
            .await
            .expect("edition")
            .map(|edition| edition.release_mbid),
        plays: droppedneedle_catalog::local::store::LocalCatalog::new(db.clone())
            .get_album_by_release_group(group)
            .await
            .expect("album by group")
            .map(|album| album.id),
        downloads: droppedneedle_media::stream::local_files::FileCatalog::new(db.clone())
            .album_by_mbid(group)
            .await
            .expect("album files by group"),
        in_discover_library: droppedneedle_catalog::discover::page::library::LibraryReads::new(
            db.clone(),
        )
        .identified_albums(1000)
        .await
        .expect("discover's library")
        .iter()
        .any(|album| album.release_group_mbid == group),
    }
}

fn owned_by(album: &str) -> Answers {
    Answers {
        holds: true,
        owned: true,
        in_owned_set: true,
        discover_links: Some(album.to_owned()),
        page_opens: Some(album.to_owned()),
        status: vec![album.to_owned()],
        pinned: None,
        plays: Some(album.to_owned()),
        downloads: Some(album.to_owned()),
        in_discover_library: false,
    }
}

fn owned_by_nothing() -> Answers {
    Answers {
        holds: false,
        owned: false,
        in_owned_set: false,
        discover_links: None,
        page_opens: None,
        status: Vec::new(),
        pinned: None,
        plays: None,
        downloads: None,
        in_discover_library: false,
    }
}

// A person took the match off: the files still name the group, but no
// reader counts the album as owning it, so a want stays open.
#[tokio::test]
async fn a_person_detached_album_settles_no_want_and_shows_no_ownership() {
    let g = group(1);
    let world = world(album(
        "al-detached",
        None,
        Some(("unmatched", "DETACHED_BY_PERSON")),
        &[Some(&g), Some(&g)],
    ))
    .await;
    assert_eq!(answers(&world, &g).await, owned_by_nothing());
}

// A recheck withdrew the match: its tags do not bring it back.
#[tokio::test]
async fn a_withdrawn_match_settles_no_want() {
    let g = group(1);
    let world = world(album(
        "al-withdrawn",
        None,
        Some(("unmatched", "IDENTITY_FAILED_RECHECK")),
        &[Some(&g), Some(&g)],
    ))
    .await;
    assert_eq!(answers(&world, &g).await, owned_by_nothing());
}

// A guess outside the group the files name fulfils no want of the group
// it guessed, hides nothing in Discover (Missing Essentials and the mixes
// included), pins no download to its release, and neither plays nor
// downloads by that group.
#[tokio::test]
async fn a_named_group_guess_fulfils_no_want_hides_nothing_and_pins_nothing() {
    let (guessed, named) = (group(1), group(2));
    let world = world(album(
        "al-guess",
        Some((&guessed, &release(1))),
        Some(("unconfirmed", "NAMED_GROUP_DIFFERS")),
        &[Some(&named), Some(&named)],
    ))
    .await;
    assert_eq!(answers(&world, &guessed).await, owned_by_nothing());
}

// The same album owns the group all its files name, and opens there.
#[tokio::test]
async fn a_named_group_guess_counts_for_the_group_its_tags_name() {
    let (guessed, named) = (group(1), group(2));
    let world = world(album(
        "al-guess",
        Some((&guessed, &release(1))),
        Some(("unconfirmed", "NAMED_GROUP_DIFFERS")),
        &[Some(&named), Some(&named)],
    ))
    .await;
    assert_eq!(answers(&world, &named).await, owned_by("al-guess"));
}

// A doubted guess inside the group still owns it and keeps its edition.
#[tokio::test]
async fn a_same_group_guess_still_owns() {
    let g = group(1);
    let world = world(album(
        "al-weak",
        Some((&g, &release(1))),
        Some(("unconfirmed", "WEAK_MATCH")),
        &[Some(&g)],
    ))
    .await;
    assert_eq!(
        answers(&world, &g).await,
        Answers {
            pinned: Some(release(1)),
            in_discover_library: true,
            ..owned_by("al-weak")
        }
    );
}

// An album identification has not reached owns the group its files name
// (a file with no id names nothing), and the catalog opens it.
#[tokio::test]
async fn a_tag_only_album_is_owned_and_opens_on_catalog_and_acquire_alike() {
    let g = group(1);
    let world = world(album("al-tags", None, None, &[Some(&g), None])).await;
    assert_eq!(answers(&world, &g).await, owned_by("al-tags"));
}

// One file naming another group leaves the album owning neither.
#[tokio::test]
async fn one_stray_tag_owns_nothing() {
    let (g, stray) = (group(1), group(2));
    let world = world(album("al-mixed", None, None, &[Some(&g), Some(&stray)])).await;
    assert_eq!(answers(&world, &g).await, owned_by_nothing());
    assert_eq!(answers(&world, &stray).await, owned_by_nothing());
}

// Every reader gives one answer in every case, and an owned group always
// opens an album.
#[tokio::test]
async fn acquisition_and_the_catalog_agree_on_every_kind_of_album() {
    let g = |n| group(n);
    let cases = [
        album("al-plain", Some((&g(1), &release(1))), None, &[None]),
        album("al-tags", None, None, &[Some(&g(2)), Some(&g(2))]),
        album("al-mixed", None, None, &[Some(&g(3)), Some(&g(4))]),
        album(
            "al-detached",
            None,
            Some(("unmatched", "DETACHED_BY_PERSON")),
            &[Some(&g(5))],
        ),
        album(
            "al-nothing-found",
            None,
            Some(("unmatched", "NO_CANDIDATE")),
            &[Some(&g(6))],
        ),
        album(
            "al-guess",
            Some((&g(7), &release(7))),
            Some(("unconfirmed", "NAMED_GROUP_DIFFERS")),
            &[Some(&g(8))],
        ),
        album(
            "al-weak",
            Some((&g(9), &release(9))),
            Some(("unconfirmed", "WEAK_MATCH")),
            &[Some(&g(10))],
        ),
        album("al-junk", None, None, &[Some(&g(11)), Some("not-an-id")]),
        album("al-no-files", Some((&g(12), &release(12))), None, &[]),
        album("al-retired", None, None, &[Some(&g(13))])
            + "UPDATE local_albums SET retired_into_album_id = 'al-plain' \
               WHERE id = 'al-retired';",
    ];
    let world = world(cases.concat()).await;
    let mut owned = Vec::new();
    for n in 1..=13 {
        let said = answers(&world, &g(n)).await;
        let all = [
            said.holds,
            said.owned,
            said.in_owned_set,
            said.discover_links.is_some(),
        ];
        assert!(
            all.iter().all(|answer| *answer == said.holds),
            "group {n}: {said:?}"
        );
        assert_eq!(said.plays, said.page_opens, "group {n} plays: {said:?}");
        assert_eq!(
            said.downloads,
            said.page_opens.clone().filter(|_| said.holds),
            "group {n} downloads: {said:?}"
        );
        if said.holds {
            assert!(
                said.page_opens.is_some() && !said.status.is_empty(),
                "group {n} opens: {said:?}"
            );
            owned.push(n);
        }
    }
    assert_eq!(owned, vec![1, 2, 8, 9, 11]);
}
