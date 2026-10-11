//! Discovery batch journey: a batch files real album requests under the
//! normal approval rules, skips what someone already asked for, stays
//! private to its owner, and its removal cancels only what it caused.

use std::sync::Arc;

use droppedneedle::acquire::requests::RequestsState;
use droppedneedle::acquire::requests::discovery::DiscoveryBatchRequests;
use droppedneedle::auth::sqlite::{AuthDb, SqliteUserStore};
use droppedneedle::reads::discover::adapters::batches::{LiveBatches, RequestsSlot};
use droppedneedle::reads::discover::models::{DiscoveryBatchCreate, DiscoveryBatchItemIn};
use droppedneedle::reads::discover::ports::{BatchFailure, BatchStore};

const MBID_A: &str = "11111111-1111-4111-8111-111111111111";
const MBID_B: &str = "22222222-2222-4222-8222-222222222222";
const MBID_C: &str = "33333333-3333-4333-8333-333333333333";

fn batch(name: &str, ids: &[&str]) -> DiscoveryBatchCreate {
    DiscoveryBatchCreate {
        name: name.to_owned(),
        source_section: "fresh_releases".to_owned(),
        items: ids
            .iter()
            .map(|id| DiscoveryBatchItemIn {
                release_group_mbid: (*id).to_owned(),
                artist_mbid: String::new(),
                album_name: format!("Album {}", &id[..2]),
                artist_name: "Someone".to_owned(),
            })
            .collect(),
    }
}

#[tokio::test]
async fn batch_files_requests_and_removal_cancels_only_its_own() {
    let (requests, db, _dispatch) = RequestsState::for_tests().expect("requests state");
    for (id, name) in [("u-ada", "Ada"), ("u-bob", "Bob")] {
        db.add_user(id, name, "user").await.expect("user row");
    }
    let slot = RequestsSlot::default();
    assert!(
        slot.set(Arc::new(DiscoveryBatchRequests::new(requests, None)))
            .is_ok()
    );
    let users = Arc::new(SqliteUserStore::new(&AuthDb::new(db.pool(), db.lane())));
    let batches = LiveBatches::new(db.pool().clone(), db.lane().clone(), users, slot);

    // Bob asks for C first; Ada's batch then skips it as a duplicate.
    let bobs = batches
        .create("u-bob", &batch("Bob's", &[MBID_C]))
        .await
        .expect("bob's batch");
    let adas = batches
        .create("u-ada", &batch("", &[MBID_A, MBID_B, MBID_C, MBID_A]))
        .await
        .expect("ada's batch");
    assert_eq!(adas.name, "Discovery batch");
    assert_eq!(adas.item_count, 3);
    let outcome = |id: &str| {
        adas.items
            .iter()
            .find(|item| item.release_group_mbid == id)
            .map(|item| (item.outcome.clone(), item.request_status.clone()))
    };
    // A plain user's requests wait for approval, as any request would.
    let waiting = Some("awaiting_approval".to_owned());
    assert_eq!(
        outcome(MBID_A),
        Some(("requested".to_owned(), waiting.clone()))
    );
    assert_eq!(
        outcome(MBID_B),
        Some(("requested".to_owned(), waiting.clone()))
    );
    assert_eq!(
        outcome(MBID_C),
        Some(("skipped_duplicate".to_owned(), waiting.clone()))
    );
    assert_eq!(adas.pending_count, 2);

    // Only already-owned or already-requested albums: nothing to file.
    assert!(matches!(
        batches.create("u-ada", &batch("Again", &[MBID_C])).await,
        Err(BatchFailure::Invalid(_))
    ));

    // Another user's batch reads as missing, and stays out of their list.
    assert_eq!(
        batches.detail("u-bob", &adas.id).await.err(),
        Some(BatchFailure::NotFound)
    );
    assert_eq!(
        batches.remove("u-bob", &adas.id, true).await.err(),
        Some(BatchFailure::NotFound)
    );
    let listed = batches.list("u-bob").await.expect("bob's list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, bobs.id);

    // Removing Ada's batch cancels her two requests and leaves Bob's.
    let removed = batches
        .remove("u-ada", &adas.id, true)
        .await
        .expect("removal");
    assert_eq!(
        (
            removed.cancelled_requests,
            removed.removed_albums,
            removed.kept
        ),
        (2, 0, 1)
    );
    assert!(batches.list("u-ada").await.expect("ada's list").is_empty());
    let bob = batches
        .detail("u-bob", &bobs.id)
        .await
        .expect("bob's batch");
    assert_eq!(bob.items[0].request_status, waiting);
}
