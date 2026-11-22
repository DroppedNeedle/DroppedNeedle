//! ListenBrainz reads follow the admin's switch: on unless an admin turns
//! them off, an old stored instance account never turns them off, and a
//! section that does not read reads nothing. A read lends the person's own
//! link when they have one, genre answers are kept, and Discover reads
//! nothing from ListenBrainz while it is off.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::discover::listening::{Ask, ListeningData};
use droppedneedle_catalog::discover::models::PromptService;
use droppedneedle_catalog::discover::wiring::live_network;
use droppedneedle_catalog::enrichment::library::LiveEnrichment;
use droppedneedle_catalog::lyrics::OnlineLyrics;
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
use droppedneedle_catalog::search::enrichment::SearchCounts;
use droppedneedle_catalog::search::models::{ArtistEnrichmentRequest, EnrichmentBatchRequest};
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::runtime_config::sections::{ListenBrainzSettings, ListenSource};
use droppedneedle_library::management::enrich::{EnrichmentPort as _, GenreAnswer};
use droppedneedle_providers::executor::Method;
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
use droppedneedle_providers::{
    ProviderEndpoints, Providers, RequestPriority, ReqwestWire, listenbrainz,
};

use crate::common::auth::AuthRig;
use crate::common::{PATIENCE, ScratchDir};
use crate::discover_queue::{discover_deps, discover_inputs};

const USER: &str = "user-ada";
/// OK Computer, the release group the recorded genre answer is for.
const ALBUM: &str = "b1392450-e666-3926-a536-22c65f834433";
const COUNTS: &str = include_str!("../fixtures/listenbrainz/popularity_artist_post.json");
const GROUPS: &str = include_str!("../fixtures/listenbrainz/top_release_groups_radiohead.json");
const GENRES: &str = include_str!("../fixtures/listenbrainz/release_group_metadata.json");
const SITEWIDE: &str = include_str!("../fixtures/listenbrainz/sitewide_artists.json");
/// Radiohead, whose top albums the recorded answer holds.
const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const MUSICBRAINZ: &str = "https://musicbrainz.test/ws/2";
/// Five of Radiohead's release groups, as recorded from a browse.
const BROWSE: &str = include_str!("../fixtures/musicbrainz/browse_release_groups_radiohead.json");
const GROUP_COUNTS: &str =
    include_str!("../fixtures/listenbrainz/popularity_release_group_radiohead_post.json");

/// The recorded browse page as a whole discography of five.
fn whole_discography() -> String {
    let mut page: serde_json::Value = serde_json::from_str(BROWSE).expect("fixture parses");
    page["release-group-count"] = 5.into();
    page.to_string()
}

fn endpoints() -> ProviderEndpoints {
    ProviderEndpoints {
        listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
        listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
        musicbrainz: MUSICBRAINZ.to_owned(),
        brainzmash: MUSICBRAINZ.to_owned(),
        ..ProviderEndpoints::closed()
    }
}

fn seed(n: u32) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

fn lb(path: &str) -> String {
    format!("{}/1/{path}", listenbrainz::DEFAULT_BASE_URL)
}

fn labs() -> String {
    format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL)
}

/// A labs list naming Radiohead alone, in the recorded rows' shape.
fn like_radiohead() -> String {
    serde_json::json!([{"artist_mbid": RADIOHEAD, "name": "Radiohead", "score": 10,
        "reference_mbid": null}])
    .to_string()
}

/// The catalog, search counts and genre lookups over `rig`'s live
/// settings, sending to a scripted ListenBrainz.
struct Reads {
    catalog: Catalog,
    counts: SearchCounts,
    genres: LiveEnrichment,
    wire: Arc<ScriptedWire>,
}

impl Reads {
    fn new(rig: &AuthRig) -> Self {
        Self::over(rig, &[whole_discography()])
    }

    /// The reads with MusicBrainz answering Radiohead's discography with
    /// `discographies` in turn, the last one again after that.
    fn over(rig: &AuthRig, discographies: &[String]) -> Self {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &lb(&format!(
                "popularity/top-release-groups-for-artist/{RADIOHEAD}"
            )),
            200,
            &[],
            GROUPS,
        );
        wire.answer(&lb("metadata/release_group/"), 200, &[], GENRES);
        wire.on(
            Method::Post,
            &lb("popularity/artist"),
            Scripted::Answer {
                status: 200,
                headers: Vec::new(),
                body: COUNTS.as_bytes().to_vec(),
            },
        );
        for discography in discographies {
            wire.answer(
                &format!("{MUSICBRAINZ}/release-group"),
                200,
                &[],
                discography.clone(),
            );
        }
        wire.on(
            Method::Post,
            &lb("popularity/release-group"),
            Scripted::Answer {
                status: 200,
                headers: Vec::new(),
                body: GROUP_COUNTS.as_bytes().to_vec(),
            },
        );
        let providers = Arc::new(Providers::unpaced_at(wire.clone(), endpoints()));
        let listening = crate::common::listening_store(&rig.platform);
        let upstream = || {
            Upstream::new(
                providers.clone(),
                Arc::new(LiveCatalogSettings::new(&rig.config)),
                rig.deps.clone(),
                listening.clone(),
            )
        };
        let catalog = Catalog::new(
            upstream(),
            LocalCatalog::new(rig.platform.db.clone()),
            crate::common::bundles::quiet_page_sources(crate::common::bundles::cover_warm_at(
                &rig.platform.db,
                rig.platform.clock.clone(),
            )),
        );
        let counts = SearchCounts::new(
            &providers,
            rig.config.live(),
            rig.config.live(),
            listening.clone(),
            rig.deps.clone(),
        );
        let genres = LiveEnrichment::new(
            upstream(),
            droppedneedle_catalog::test_support::artwork(&rig.platform, &providers),
            OnlineLyrics::new(&providers, rig.config.live()),
            providers.acoustid.clone(),
            rig.config.live(),
        );
        Self {
            catalog,
            counts,
            genres,
            wire,
        }
    }

    /// Queue `body` as labs' next answer; the last one queued repeats.
    fn labs(&self, body: &str) {
        self.wire.answer(&labs(), 200, &[], body);
    }

    /// Requests sent to ListenBrainz and its labs host so far.
    fn sent(&self) -> usize {
        self.wire
            .sent()
            .iter()
            .filter(|sent| {
                let url = sent.url.as_str();
                url.starts_with(listenbrainz::DEFAULT_BASE_URL)
                    || url.starts_with(listenbrainz::LABS_BASE_URL)
            })
            .count()
    }

    /// The authorization headers of the listener-count reads so far.
    fn lent(&self) -> Vec<Option<String>> {
        self.lent_to("popularity/artist")
    }

    /// The authorization headers of the reads to paths ending `path`.
    fn lent_to(&self, path: &str) -> Vec<Option<String>> {
        self.wire
            .sent()
            .iter()
            .filter(|sent| sent.url.path().ends_with(path))
            .map(|sent| {
                sent.headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                    .map(|(_, value)| value.clone())
            })
            .collect()
    }
}

/// With no row, or an old row whose instance account is switched off,
/// recommendations read ListenBrainz. Switched off, the album and artist
/// shelves say so, genres answer "off", search has no ListenBrainz counts
/// and nothing is sent. A row that does not read sends nothing either, and
/// genres stay unanswered so the work waits. Switched back on, the shelf
/// the off answers left empty fills.
#[tokio::test]
async fn listenbrainz_reads_are_on_unless_switched_off() {
    let rig = AuthRig::new("listenbrainz-reads").await;
    rig.seed_user("ada", Role::User).await;
    let reads = Reads::new(&rig);
    let store = |text: &str| {
        rig.config
            .replace_stored_for_test("listenbrainz_settings", text)
            .expect("stored");
    };

    for (case, stored, n) in [
        ("no row", None, 1),
        (
            "old instance account",
            Some(r#"{"enabled": false, "username": "x", "user_token": ""}"#),
            2,
        ),
    ] {
        if let Some(text) = stored {
            store(text);
        }
        reads.labs(&like_radiohead());
        let before = reads.sent();
        let similar = reads
            .catalog
            .similar_albums(USER, ALBUM, &seed(n), 4)
            .await
            .unwrap();
        assert_eq!(
            (similar.source, similar.configured),
            (ListenSource::Listenbrainz, true),
            "{case}"
        );
        assert!(!similar.albums.is_empty(), "{case}");
        assert!(reads.sent() > before, "{case}: ListenBrainz was asked");
    }

    for (case, stored) in [
        ("switched off", r#"{"reads_enabled": false}"#),
        ("does not read", r#"{"reads_enabled": "yes"}"#),
    ] {
        store(stored);
        let before = reads.sent();
        let similar = reads
            .catalog
            .similar_albums(USER, ALBUM, &seed(9), 4)
            .await
            .unwrap();
        assert_eq!(
            (similar.source, similar.configured),
            (ListenSource::Listenbrainz, false),
            "{case}"
        );
        let section = reads
            .catalog
            .similar_artists(USER, &seed(9), 5, None, RequestPriority::UserInitiated)
            .await
            .unwrap();
        assert_eq!(
            (section.source, section.configured),
            (ListenSource::Listenbrainz, false),
            "{case}"
        );
        let counts = reads
            .counts
            .enrich(
                USER,
                EnrichmentBatchRequest {
                    artists: vec![ArtistEnrichmentRequest {
                        musicbrainz_id: seed(9),
                        name: "Someone".to_owned(),
                    }],
                    albums: Vec::new(),
                },
            )
            .await;
        assert_eq!(counts.source, None, "{case}");
        let genres = reads.genres.listenbrainz_genres(ALBUM, false).await;
        if case == "switched off" {
            assert_eq!(genres, Ok(GenreAnswer::Off), "{case}");
        } else {
            assert!(genres.is_err(), "{case}: genres stay unanswered");
        }
        assert_eq!(reads.sent(), before, "{case}: nothing reached ListenBrainz");
    }

    store("{}");
    let before = reads.sent();
    let back_on = reads
        .catalog
        .similar_albums(USER, ALBUM, &seed(9), 4)
        .await
        .unwrap();
    assert!(back_on.configured);
    assert!(!back_on.albums.is_empty(), "no empty answer was kept");
    assert!(reads.sent() > before, "ListenBrainz was asked");
}

/// A read lends the person's own ListenBrainz link when they have one and
/// goes out anonymous otherwise.
#[tokio::test]
async fn reads_lend_the_persons_own_link() {
    let rig = AuthRig::new("listenbrainz-lend").await;
    rig.seed_user("ada", Role::User).await;
    rig.seed_user("bob", Role::User).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(USER, "ada-lb", "ada-token")
        .await
        .expect("link");
    let reads = Reads::new(&rig);
    // Each artist has a neighbour of its own, so each read counts one.
    let asks = [(USER, seed(3), seed(30)), ("user-bob", seed(4), seed(40))];
    for (_, artist, neighbour) in &asks {
        reads.labs(
            &serde_json::json!([{"artist_mbid": neighbour, "name": "Someone",
                "score": 10, "reference_mbid": artist}])
            .to_string(),
        );
    }
    for (user, artist, _) in &asks {
        reads
            .catalog
            .similar_artists(user, artist, 5, None, RequestPriority::UserInitiated)
            .await
            .unwrap();
    }
    assert_eq!(
        reads.lent(),
        [Some("Token ada-token".to_owned()), None],
        "an unlinked person reads anonymous"
    );
}

/// ListenBrainz gives an artist's top albums only to a linked account. An
/// unlinked person's artist page sends nothing, keeps nothing under the
/// albums' shared key and says to link rather than that ListenBrainz is
/// down; a linked person reading next still gets the albums from
/// ListenBrainz.
#[tokio::test]
async fn an_unlinked_read_keeps_nothing_a_linked_one_needs() {
    let rig = AuthRig::new("listenbrainz-unlinked").await;
    rig.seed_user("ada", Role::User).await;
    rig.seed_user("bob", Role::User).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(USER, "ada-lb", "ada-token")
        .await
        .expect("link");
    let reads = Reads::new(&rig);
    let artist = RADIOHEAD;
    let top = |user: &'static str| {
        reads.catalog.top_albums(
            user,
            artist,
            5,
            Some(ListenSource::Listenbrainz),
            RequestPriority::UserInitiated,
        )
    };

    let unlinked = top("user-bob").await.unwrap();
    assert!(unlinked.albums.is_empty());
    assert_eq!(
        (&unlinked.needs_account, &unlinked.unavailable_sources),
        (&vec![ListenSource::Listenbrainz], &Vec::new()),
        "an account is needed, nothing is down"
    );
    assert_eq!(reads.sent(), 0, "nothing was sent without a token");

    let linked = top(USER).await.unwrap();
    assert_eq!(linked.source, ListenSource::Listenbrainz);
    assert!(!linked.albums.is_empty(), "{linked:?}");
    assert!(linked.needs_account.is_empty());
    assert_eq!(reads.sent(), 1, "nothing was kept, so the read went out");
}

/// No album list lends a token, the listening data's or the album page's,
/// for a linked person or anyone else. With ListenBrainz reads off the
/// list still answers, newest first, and nothing is sent to ListenBrainz.
#[tokio::test]
async fn album_lists_lend_no_token_and_answer_with_listenbrainz_off() {
    let rig = AuthRig::new("listenbrainz-album-lists").await;
    rig.seed_user("ada", Role::User).await;
    rig.seed_user("bob", Role::User).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(USER, "ada-lb", "ada-token")
        .await
        .expect("link");
    let reads = Reads::new(&rig);
    let dir = ScratchDir::new("listenbrainz-album-lists");
    // Its own cache, so the album page's read below is sent as well.
    let inputs = discover_inputs(
        &rig,
        &dir,
        Providers::unpaced_at(reads.wire.clone(), endpoints()),
    );
    let listening = ListeningData::new(
        live_network(&inputs, &rig.config),
        inputs.providers.cache.clone(),
    );

    for user in [USER, "user-bob"] {
        let albums = listening
            .artist_albums(user, RADIOHEAD, 10, &Ask::person())
            .await
            .expect("the albums read");
        assert_eq!(albums.len(), 5, "{user}");
    }
    reads.labs(&like_radiohead());
    let similar = reads
        .catalog
        .similar_albums(USER, ALBUM, &seed(1), 4)
        .await
        .unwrap();
    assert!(!similar.albums.is_empty());
    let lent = reads.lent_to("popularity/release-group");
    assert_eq!(lent, [None, None], "one count read each, with no token");

    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: false,
        })
        .expect("off");
    let before = reads.sent();
    let dated: Vec<String> = listening
        .artist_albums(USER, RADIOHEAD, 10, &Ask::person())
        .await
        .expect("the albums read")
        .into_iter()
        .map(|row| row.album.title)
        .collect();
    assert_eq!(
        dated,
        [
            "Kid A",
            "OK Computer",
            "Radiotick Tracks 2",
            "The Bends",
            "Pablo Honey"
        ]
    );
    assert_eq!(reads.sent(), before, "nothing reached ListenBrainz");
}

/// An album page built while a similar artist's discography is still
/// being read is shown without that artist's albums and is not kept: the
/// next visit has them.
#[tokio::test]
async fn an_album_page_built_before_a_discography_is_whole_is_not_kept() {
    let rig = AuthRig::new("listenbrainz-album-page-partial").await;
    rig.seed_user("ada", Role::User).await;
    // The recorded page as answered: 5 of 587 groups, so the rest is read
    // in the background.
    let reads = Reads::over(&rig, &[BROWSE.to_owned(), whole_discography()]);
    reads.labs(&like_radiohead());

    let first = reads
        .catalog
        .similar_albums(USER, ALBUM, &seed(2), 4)
        .await
        .unwrap();
    assert!(first.albums.is_empty(), "{first:?}");
    let next = reads
        .catalog
        .similar_albums(USER, ALBUM, &seed(2), 4)
        .await
        .unwrap();
    assert!(!next.albums.is_empty(), "the first answer was not kept");
}

/// Two genre plans for the same album ask ListenBrainz once.
#[tokio::test]
async fn genre_answers_are_kept() {
    let rig = AuthRig::new("listenbrainz-genres").await;
    let reads = Reads::new(&rig);
    let first = reads.genres.listenbrainz_genres(ALBUM, true).await;
    let second = reads.genres.listenbrainz_genres(ALBUM, true).await;
    assert!(matches!(&first, Ok(GenreAnswer::Genres(tags)) if !tags.is_empty()));
    assert_eq!(first, second);
    assert_eq!(reads.sent(), 1, "the second plan read the kept answer");
}

/// While ListenBrainz reads are off a Discover build sends nothing to
/// ListenBrainz and a linked person is not asked to connect it; switched
/// back on, the page fills from it.
#[tokio::test]
async fn discover_reads_no_listenbrainz_while_it_is_off() {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let stub = Router::new().fallback(move |uri: Uri| {
        let seen = seen.clone();
        async move {
            let path = uri.path().to_owned();
            if !path.starts_with("/listenbrainz/") && !path.starts_with("/labs/") {
                return StatusCode::NOT_FOUND.into_response();
            }
            seen.lock().unwrap().push(path.clone());
            if path.ends_with("/1/stats/sitewide/artists") {
                ([("content-type", "application/json")], SITEWIDE).into_response()
            } else {
                StatusCode::NO_CONTENT.into_response()
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let http = HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&http)),
        ProviderEndpoints {
            listenbrainz: format!("{origin}/listenbrainz"),
            listenbrainz_labs: format!("{origin}/labs"),
            musicbrainz: format!("{origin}/musicbrainz"),
            lastfm: format!("{origin}/lastfm/"),
            ..ProviderEndpoints::closed()
        },
    );
    let rig = AuthRig::new("discover-listenbrainz-off").await;
    let user = rig.seed_user("1", Role::User).await;
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(&user.id, "listener", "token")
        .await
        .expect("link");
    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: false,
        })
        .expect("off");
    let dir = ScratchDir::new("discover-listenbrainz-off");
    let content = discover_deps(&rig, &dir, providers, |_| {}).content;
    // A build ends with no commit after its in-flight mark clears, so this
    // asks until the page says it has settled.
    let settled = || async {
        tokio::time::timeout(PATIENCE, async {
            loop {
                let page = content.discover(&user.id).await.expect("page");
                if !page.refreshing {
                    return page;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the build finished")
    };

    content.discover(&user.id).await.expect("page");
    let off = settled().await;
    assert!(
        asked.lock().unwrap().is_empty(),
        "{:?}",
        asked.lock().unwrap()
    );
    assert!(
        !off.service_prompts
            .iter()
            .any(|prompt| prompt.service == PromptService::Listenbrainz),
        "a linked person is not asked to connect ListenBrainz"
    );

    rig.config
        .save_blocking(ListenBrainzSettings {
            reads_enabled: true,
        })
        .expect("on");
    content.trigger_refresh(&user.id).await.expect("refresh");
    let on = settled().await;
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.ends_with("/1/stats/sitewide/artists")),
        "the build read ListenBrainz again"
    );
    assert!(!on.sections.is_empty(), "the shelves fill");
}
