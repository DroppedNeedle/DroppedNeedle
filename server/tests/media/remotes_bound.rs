//! The per-source bound on the remote reads people trigger: API answers
//! queue past the bound, images never wait behind them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::Uri;
use droppedneedle_catalog::reads::collections::remote_import::PlaylistImportSink;
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::JellyfinConnection;
use droppedneedle_media::remotes::adapter::USER_READS_PER_SOURCE;
use droppedneedle_media::remotes::folders::SqliteFolderStore;
use droppedneedle_media::remotes::models::SourceName;
use droppedneedle_media::remotes::service::RemotesService;
use droppedneedle_media::remotes::warmup::MbidIndex;
use tokio::sync::Semaphore;

/// A Jellyfin that counts API and image requests and holds every API
/// answer until `gate` opens.
async fn held_jellyfin(
    api: Arc<AtomicUsize>,
    images: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
) -> String {
    let app = Router::new().fallback(move |uri: Uri| {
        let (api, images, gate) = (api.clone(), images.clone(), gate.clone());
        async move {
            if uri.path().contains("/Images/") {
                images.fetch_add(1, Ordering::SeqCst);
                return ([("content-type", "image/jpeg")], vec![1u8, 2, 3]);
            }
            api.fetch_add(1, Ordering::SeqCst);
            let _open = gate.acquire().await;
            (
                [("content-type", "application/json")],
                br#"{"Items":[]}"#.to_vec(),
            )
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake binds");
    let address = listener.local_addr().expect("fake has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// Wait, bounded in wall time, until `count` reaches `want`.
async fn reaches(count: &AtomicUsize, want: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while count.load(Ordering::SeqCst) < want {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the requests arrive");
}

#[tokio::test]
async fn a_persons_api_reads_queue_past_the_bound_but_images_never_wait() {
    let dir = crate::common::ScratchDir::new("remotes-bound");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .build()
        .await;
    let (api, images) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let gate = Arc::new(Semaphore::new(0));
    let base_url = held_jellyfin(api.clone(), images.clone(), gate.clone()).await;
    platform
        .settings
        .save_secret(JellyfinConnection {
            jellyfin_url: base_url,
            api_key: Secret::new("key"),
            user_id: "jf-user".to_owned(),
            enabled: true,
            ..JellyfinConnection::default()
        })
        .await
        .expect("jellyfin saves");
    let service = RemotesService::new(
        reqwest::Client::new(),
        Arc::new(crate::common::bundles::remotes_resolver(&platform)),
        Arc::new(SqliteFolderStore::new(
            platform.db.clone(),
            platform.clock.clone(),
        )),
        Arc::new(PlaylistImportSink::new(
            crate::common::bundles::collections_at(&platform).wired(),
        )),
        MbidIndex::default(),
        platform.clock.clone(),
    );

    let reads: Vec<_> = (0..=USER_READS_PER_SOURCE)
        .map(|_| {
            let service = service.clone();
            tokio::spawn(async move { service.genres("user-ada", SourceName::Jellyfin).await })
        })
        .collect();
    reaches(&api, USER_READS_PER_SOURCE).await;

    let image = tokio::time::timeout(
        Duration::from_secs(10),
        service.image("user-ada", SourceName::Jellyfin, "item-1".to_owned(), 300),
    )
    .await
    .expect("an image never waits behind held API reads");
    assert!(image.is_ok(), "{image:?}");
    assert_eq!(images.load(Ordering::SeqCst), 1);
    assert_eq!(
        api.load(Ordering::SeqCst),
        USER_READS_PER_SOURCE,
        "the read past the bound waits for a place"
    );

    gate.add_permits(USER_READS_PER_SOURCE + 1);
    for read in reads {
        let genres = read.await.expect("read task runs");
        assert_eq!(genres, Ok(Vec::new()));
    }
    assert_eq!(api.load(Ordering::SeqCst), USER_READS_PER_SOURCE + 1);
    drop(platform);
    runtime.shutdown().await;
}
