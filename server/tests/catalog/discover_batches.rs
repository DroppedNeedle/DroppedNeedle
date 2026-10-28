//! Discovery batch journey: a batch files real album requests under the
//! normal approval rules, skips what someone already asked for, stays
//! private to its owner, and its removal cancels only what it caused and
//! never recycles a plain user's albums.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use droppedneedle_acquire::requests::RequestsState;
use droppedneedle_acquire::requests::discovery::DiscoveryBatchRequests;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Admin, Principal, Role};
use droppedneedle_catalog::reads::discover::batches::{BatchAlbum, BatchRequests, LiveBatches};
use droppedneedle_catalog::reads::discover::models::{
    BatchOutcome, DiscoveryBatchCreate, DiscoveryBatchItemIn,
};
use droppedneedle_catalog::reads::discover::ports::{BatchFailure, BoxFuture};

use crate::common::auth::test_principal;

const MBID_A: &str = "11111111-1111-4111-8111-111111111111";
const MBID_B: &str = "22222222-2222-4222-8222-222222222222";
const MBID_C: &str = "33333333-3333-4333-8333-333333333333";

/// A plain user signed in, as the session gate would hand the handler.
fn user(user_id: &str) -> Principal {
    test_principal(user_id, Role::User, "sess-batches", SessionKind::Standard)
}

fn batch(name: &str, ids: &[&str]) -> DiscoveryBatchCreate {
    DiscoveryBatchCreate {
        name: name.to_owned(),
        source_section: "fresh_releases".to_owned(),
        items: ids
            .iter()
            .map(|id| DiscoveryBatchItemIn {
                musicbrainz_id: (*id).to_owned(),
                artist_id: String::new(),
                title: format!("Album {}", &id[..2]),
                artist_name: "Someone".to_owned(),
            })
            .collect(),
        idempotency_key: None,
    }
}

/// The real requests side, counting every recycle it is asked for.
struct Counting {
    inner: DiscoveryBatchRequests,
    recycles: AtomicUsize,
}

impl BatchRequests for Counting {
    fn request_albums<'a>(
        &'a self,
        caller: &'a Principal,
        albums: Vec<BatchAlbum>,
    ) -> BoxFuture<'a, Result<(), BatchFailure>> {
        self.inner.request_albums(caller, albums)
    }
    fn withdraw_album<'a>(
        &'a self,
        owner_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        self.inner.withdraw_album(owner_id, release_group_mbid)
    }
    fn remove_album<'a>(
        &'a self,
        admin: &'a Admin,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        self.recycles.fetch_add(1, Ordering::SeqCst);
        self.inner.remove_album(admin, release_group_mbid)
    }
}

#[tokio::test]
async fn batch_files_requests_and_removal_cancels_only_its_own() {
    let (requests, db, _dispatch) = RequestsState::for_tests().expect("requests state");
    for (id, name) in [("u-ada", "Ada"), ("u-bob", "Bob")] {
        db.add_user(id, name, "user").await.expect("user row");
    }
    let counting = Arc::new(Counting {
        inner: DiscoveryBatchRequests::new(requests, None),
        recycles: AtomicUsize::new(0),
    });
    let batches = LiveBatches::new(db.db().clone(), db.clock().clone(), Some(counting.clone()));

    // Bob asks for C first; Ada's batch then skips it as a duplicate.
    let bobs = batches
        .create(&user("u-bob"), &batch("Bob's", &[MBID_C]))
        .await
        .expect("bob's batch");
    let adas = batches
        .create(
            &user("u-ada"),
            &batch("", &[MBID_A, MBID_B, MBID_C, MBID_A]),
        )
        .await
        .expect("ada's batch");
    assert_eq!(adas.name, "Discovery batch");
    assert_eq!(adas.item_count, 3);
    let outcome = |id: &str| {
        adas.items
            .iter()
            .find(|item| item.musicbrainz_id == id)
            .map(|item| (item.outcome, item.request_status.clone()))
    };
    // A plain user's requests wait for approval, as any request would.
    let waiting = Some("awaiting_approval".to_owned());
    assert_eq!(
        outcome(MBID_A),
        Some((BatchOutcome::Requested, waiting.clone()))
    );
    assert_eq!(
        outcome(MBID_B),
        Some((BatchOutcome::Requested, waiting.clone()))
    );
    assert_eq!(
        outcome(MBID_C),
        Some((BatchOutcome::SkippedDuplicate, waiting.clone()))
    );
    assert_eq!(adas.pending_count, 2);

    // Only already-owned or already-requested albums: nothing to file.
    assert!(matches!(
        batches
            .create(&user("u-ada"), &batch("Again", &[MBID_C]))
            .await,
        Err(BatchFailure::Invalid(_))
    ));

    // Another user's batch reads as missing, and stays out of their list.
    assert_eq!(
        batches.detail(&user("u-bob"), &adas.id).await.err(),
        Some(BatchFailure::NotFound)
    );
    assert_eq!(
        batches.remove(&user("u-bob"), &adas.id, true).await.err(),
        Some(BatchFailure::NotFound)
    );
    let listed = batches.list("u-bob").await.expect("bob's list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, bobs.id);

    // Album A lands in the library.
    db.write("test.land_album", |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('artist-a', 'Someone', 'someone', 'person', 0, 0); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) VALUES \
             ('album-a', 'root-1', 'g-a', 'Album 11', 'album 11', 'artist-a', 'manual', 0, 0); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) VALUES \
             ('album-a', 'musicbrainz', '{MBID_A}', 'manual', 0); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source) VALUES ('track-a', 'album-a', 'root-1', '/music/a.flac', \
             'a.flac', 'hash-a', 1, 0, 'stat-a', 'One', 'one', 'Album 11', 'album 11', \
             'flac', 'scan', 0, 'manual');"
        ))?;
        Ok(())
    })
    .await
    .expect("landed album");

    // Removing Ada's batch cancels her pending request for B, leaves Bob's
    // C alone, and never recycles A: plain users don't recycle albums.
    let removed = batches
        .remove(&user("u-ada"), &adas.id, true)
        .await
        .expect("removal");
    assert_eq!(
        (
            removed.cancelled_requests,
            removed.removed_albums,
            removed.kept
        ),
        (1, 0, 2)
    );
    assert_eq!(counting.recycles.load(Ordering::SeqCst), 0);
    assert!(batches.list("u-ada").await.expect("ada's list").is_empty());
    let bob = batches
        .detail(&user("u-bob"), &bobs.id)
        .await
        .expect("bob's batch");
    assert_eq!(bob.items[0].request_status, waiting);
}
