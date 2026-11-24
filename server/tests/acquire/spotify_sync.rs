//! Spotify sync: a playlist imported from Spotify, or Liked Songs, takes
//! Spotify's new songs by itself. A pass asks Spotify for the playlist's
//! version first and reads its songs only when it changed; new songs are
//! added once, at the end (Liked Songs at the top), and nothing is ever
//! removed, moved or added back.

use axum::http::StatusCode;
use droppedneedle_acquire::imports::spotify_albums::{SpotifyAlbum, pick_own_group};
use droppedneedle_acquire::imports::spotify_oauth::SpotifyConnection;
use droppedneedle_providers::musicbrainz::models::RecordingSearchHit;
use droppedneedle_testkit::imports::{
    MockRecorder, MockServer, SPOTIFY_LIKED_DEFAULT, SPOTIFY_REFRESH, SPOTIFY_SCOPES,
    SPOTIFY_TOKEN, SPOTIFY_USER_ID, serve_spotify, spotify_track,
};
use droppedneedle_testkit::musicbrainz::{MockMusicBrainz, sync_group, sync_recording};
use serde_json::{Value, json};

use crate::common::{Login, TestApp, closed_outbound};

/// The app with the Spotify mocks as Spotify, its app turned on, and the
/// MusicBrainz mock as MusicBrainz.
struct Rig {
    app: TestApp,
    spotify: MockRecorder,
    musicbrainz: MockMusicBrainz,
    _servers: (MockServer, MockServer, MockServer),
}

async fn rig(tag: &str) -> Rig {
    let (api, accounts, covers, spotify) = serve_spotify().await.expect("the mock serves");
    let musicbrainz = MockMusicBrainz::start().await.expect("the mock serves");
    let app = TestApp::start_with(
        tag,
        droppedneedle::bootstrap::Outbound {
            brainzmash: musicbrainz.base_url(),
            spotify_api: api.base_url.clone(),
            spotify_accounts: accounts.base_url.clone(),
            spotify_cover_cdn: Some(covers.base_url.clone()),
            ..closed_outbound()
        },
    )
    .await;
    let admin = app.login_admin().await;
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/acquire/spotify/settings",
            &[("authorization", admin.as_str())],
            Some(json!({
                "client_id": "test-client",
                "client_secret": "test-secret",
                "enabled": true,
                "spotify_redirect_origin": "",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Rig {
        app,
        spotify,
        musicbrainz,
        _servers: (api, accounts, covers),
    }
}

impl Rig {
    /// `who`'s Spotify link, granted `scopes`, with `access_token` good
    /// for a long while.
    async fn link(&self, who: &Login, scopes: &str, access_token: &str) {
        self.app
            .state()
            .acquire
            .imports
            .spotify_links
            .upsert(
                &who.user_id,
                &SpotifyConnection {
                    access_token: access_token.to_owned(),
                    refresh_token: SPOTIFY_REFRESH.to_owned(),
                    expires_at_unix: 4_000_000_000,
                    username: "Mock Listener".to_owned(),
                    spotify_user_id: SPOTIFY_USER_ID.to_owned(),
                    scopes: scopes.to_owned(),
                },
            )
            .await
            .expect("the link saves");
    }

    async fn call(
        &self,
        who: &Login,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.app
            .call(
                method,
                uri,
                &[("authorization", who.authorization.as_str())],
                body,
            )
            .await
    }

    /// Import Spotify playlist `spotify_id` as `who` and wait for its
    /// `rows` rows.
    async fn import(&self, who: &Login, spotify_id: &str, rows: usize) -> String {
        let path = if spotify_id == "liked" {
            "/api/v3/acquire/spotify/liked-songs/import".to_owned()
        } else {
            format!("/api/v3/acquire/spotify/playlists/{spotify_id}/import")
        };
        let (status, body) = self
            .call(who, "POST", &path, Some(json!({"name": "Road Trip"})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let playlist_id = body["playlist_id"]
            .as_str()
            .expect("playlist id")
            .to_owned();
        self.app
            .until("the imported rows", || async {
                (titles(&self.app, &playlist_id).len() == rows
                    && !self.import_waiting(who, spotify_id))
                .then_some(())
            })
            .await;
        playlist_id
    }

    /// Whether `who`'s import of `spotify_id` is queued or running.
    fn import_waiting(&self, who: &Login, spotify_id: &str) -> bool {
        self.app
            .db()
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'spotify.import' \
                 AND idempotency_key = ?1 AND state IN ('queued', 'running')",
                [format!("spotify:import:{}:{spotify_id}", who.user_id)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    /// How many passes of `playlist_id` are in `state`.
    fn passes(&self, playlist_id: &str, state: &str) -> i64 {
        self.app
            .db()
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'spotify.sync' \
                 AND idempotency_key = ?1 AND state = ?2",
                [format!("spotify:sync:{playlist_id}"), state.to_owned()],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Turn the sync of `playlist_id` on or off as `who`.
    async fn switch(&self, who: &Login, playlist_id: &str, enabled: bool) -> (StatusCode, Value) {
        self.call(
            who,
            "PUT",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            Some(json!({"enabled": enabled})),
        )
        .await
    }

    /// When the last pass of `playlist_id` finished, unix ms.
    fn checked_at(&self, playlist_id: &str) -> Option<i64> {
        self.app
            .db()
            .query_row(
                "SELECT checked_at FROM acquire_playlist_syncs WHERE playlist_id = ?1",
                [playlist_id],
                |row| row.get(0),
            )
            .ok()
            .flatten()
    }

    /// The sync row's failure and kept version.
    fn failure_and_version(&self, playlist_id: &str) -> (Option<String>, Option<String>) {
        self.app
            .db()
            .query_row(
                "SELECT failure, snapshot_id FROM acquire_playlist_syncs WHERE playlist_id = ?1",
                [playlist_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the sync row reads")
    }

    /// Queue a pass of `spotify_id`'s synced playlist the way a person
    /// does, through the import route, and wait until the pass has run:
    /// it finished, or it recorded a failure.
    async fn pass(&self, who: &Login, spotify_id: &str, playlist_id: &str) {
        let before = (self.checked_at(playlist_id), self.failures(playlist_id));
        let path = if spotify_id == "liked" {
            "/api/v3/acquire/spotify/liked-songs/import".to_owned()
        } else {
            format!("/api/v3/acquire/spotify/playlists/{spotify_id}/import")
        };
        let (status, body) = self
            .call(who, "POST", &path, Some(json!({"name": "Road Trip"})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        self.app
            .until("a sync pass", || async {
                let now = (self.checked_at(playlist_id), self.failures(playlist_id));
                (now != before).then_some(())
            })
            .await;
    }

    fn failures(&self, playlist_id: &str) -> i64 {
        self.app
            .db()
            .query_row(
                "SELECT failures FROM acquire_playlist_syncs WHERE playlist_id = ?1",
                [playlist_id],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    /// The Spotify paths called from call number `mark` on.
    fn calls_since(&self, mark: usize) -> Vec<String> {
        self.spotify.snapshot().spotify_paths[mark..].to_vec()
    }

    fn mark(&self) -> usize {
        self.spotify.snapshot().spotify_paths.len()
    }
}

fn titles(app: &TestApp, playlist_id: &str) -> Vec<String> {
    app.db()
        .prepare(
            "SELECT track_name FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
             ORDER BY position",
        )
        .unwrap()
        .query_map([playlist_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Spotify track number `n`, on an album of its own.
fn song(n: u32) -> Value {
    spotify_track(
        &format!("sp-song-{n}"),
        &format!("Song {n}"),
        "Aurora Current",
        (&format!("sp-album-{n}"), &format!("Album {n}")),
        &format!("QZSYN26{n:05}"),
    )
}

/// Turning sync on reads the playlist once. A pass at the same version
/// asks Spotify one question and reads no songs; a new version adds the
/// new song at the end, once. A song removed here is not added back, and
/// one removed on Spotify stays. A save that fails keeps the old version,
/// so the next pass adds the song once. An editor may read the sync but
/// not change it.
#[tokio::test]
async fn a_synced_playlist_takes_new_songs_once_and_keeps_the_rest() {
    let rig = rig("spotify-sync-playlist").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-sync", "v1", vec![song(1), song(2)]);
    let playlist_id = rig.import(&ada, "sp-sync", 2).await;

    let mark = rig.mark();
    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    assert_eq!(
        rig.calls_since(mark),
        ["/playlists/sp-sync", "/playlists/sp-sync/items"],
        "the first pass reads the songs once"
    );
    assert_eq!(titles(&rig.app, &playlist_id), ["Song 1", "Song 2"]);

    let mark = rig.mark();
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        rig.calls_since(mark),
        ["/playlists/sp-sync"],
        "unchanged: one question, and the import route queued a pass, not an import"
    );

    rig.spotify
        .set_playlist("sp-sync", "", vec![song(1), song(2)]);
    let mark = rig.mark();
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        rig.calls_since(mark),
        ["/playlists/sp-sync", "/playlists/sp-sync/items"],
        "no version reads as changed"
    );

    rig.spotify
        .set_playlist("sp-sync", "v2", vec![song(1), song(2), song(3)]);
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        titles(&rig.app, &playlist_id),
        ["Song 1", "Song 2", "Song 3"]
    );

    let entry: String = rig
        .app
        .db()
        .query_row(
            "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
             AND track_name = 'Song 2'",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    let (status, body) = rig
        .call(
            &ada,
            "DELETE",
            &format!("/api/v3/playlists/{playlist_id}/tracks/{entry}"),
            None,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    rig.spotify
        .set_playlist("sp-sync", "v3", vec![song(2), song(3), song(4)]);
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        titles(&rig.app, &playlist_id),
        ["Song 1", "Song 3", "Song 4"],
        "a song removed here stays removed, and one removed on Spotify stays"
    );

    rig.app
        .seed(
            "CREATE TRIGGER test_refuse_songs BEFORE INSERT ON catalog_playlist_source_songs \
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .await;
    rig.spotify
        .set_playlist("sp-sync", "v4", vec![song(3), song(4), song(5)]);
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        rig.failure_and_version(&playlist_id),
        (Some("save_failed".to_owned()), Some("v3".to_owned())),
        "a failed save keeps the version before it"
    );
    rig.app.seed("DROP TRIGGER test_refuse_songs").await;
    rig.pass(&ada, "sp-sync", &playlist_id).await;
    assert_eq!(
        titles(&rig.app, &playlist_id),
        ["Song 1", "Song 3", "Song 4", "Song 5"]
    );
    assert_eq!(
        rig.failure_and_version(&playlist_id),
        (None, Some("v4".to_owned()))
    );

    let bo = rig.app.login_user("bobby").await;
    rig.app
        .seed(format!(
            "INSERT INTO catalog_playlist_members (playlist_id, user_id, added_at, can_edit) \
             VALUES ('{playlist_id}', '{}', 0, 1)",
            bo.user_id
        ))
        .await;
    let (status, body) = rig
        .call(
            &bo,
            "GET",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    let (status, _) = rig.switch(&bo, &playlist_id, false).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "only the owner switches it");

    rig.app.stop().await;
}

/// A playlist imported before its songs were kept takes its first pass's
/// songs once: the rows already there are paired with the songs they are,
/// and only the new one is added.
#[tokio::test]
async fn a_playlist_imported_before_is_seeded_without_duplicates() {
    let rig = rig("spotify-sync-seed").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-old", "v1", vec![song(1), song(2)]);
    let playlist_id = rig.import(&ada, "sp-old", 2).await;
    rig.app
        .seed(format!(
            "DELETE FROM catalog_playlist_source_songs WHERE playlist_id = '{playlist_id}'"
        ))
        .await;
    rig.spotify
        .set_playlist("sp-old", "v2", vec![song(1), song(2), song(3)]);

    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    assert_eq!(
        titles(&rig.app, &playlist_id),
        ["Song 1", "Song 2", "Song 3"]
    );
    let kept: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM catalog_playlist_source_songs s \
             JOIN catalog_playlist_tracks e ON e.id = s.entry_id WHERE s.playlist_id = ?1",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 3, "each row is kept as the song it is");

    rig.app.stop().await;
}

/// An import queued before its playlist's sync was turned on never
/// replaces what the sync keeps: the pass waits for it, and the import,
/// finding sync on, writes nothing and leaves the playlist to the pass. A
/// song taken before and since removed on Spotify stays.
#[tokio::test]
async fn an_import_queued_before_sync_leaves_the_playlist_to_it() {
    let rig = rig("spotify-sync-import-race").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-race", "v1", vec![song(1), song(2)]);
    let playlist_id = rig.import(&ada, "sp-race", 2).await;
    let payload = json!({
        "user_id": ada.user_id,
        "spotify_playlist_id": "sp-race",
        "playlist_id": playlist_id,
    });
    rig.app
        .seed(format!(
            "INSERT INTO durable_jobs (kind, idempotency_key, state, attempts, not_before_at, \
             payload_json, created_at, updated_at) VALUES ('spotify.import', \
             'spotify:import:{}:sp-race', 'queued', 0, 9000000000000, '{payload}', 0, 0)",
            ada.user_id
        ))
        .await;
    rig.spotify
        .set_playlist("sp-race", "v2", vec![song(1), song(2), song(3)]);

    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            (rig.passes(&playlist_id, "done") > 0).then_some(())
        })
        .await;
    rig.spotify
        .set_playlist("sp-race", "v3", vec![song(2), song(3)]);
    rig.app
        .seed(
            "UPDATE durable_jobs SET not_before_at = 0 \
             WHERE kind = 'spotify.import' AND state = 'queued'",
        )
        .await;
    rig.app
        .until("the import", || async {
            (!rig.import_waiting(&ada, "sp-race") && rig.passes(&playlist_id, "running") == 0)
                .then_some(())
        })
        .await;
    rig.app
        .seed(
            "UPDATE durable_jobs SET not_before_at = 0 \
             WHERE kind = 'spotify.sync' AND state = 'queued'",
        )
        .await;
    rig.pass(&ada, "sp-race", &playlist_id).await;

    assert_eq!(
        titles(&rig.app, &playlist_id),
        ["Song 1", "Song 2", "Song 3"],
        "the import replaced nothing"
    );
    let kept: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM catalog_playlist_source_songs WHERE playlist_id = ?1",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 3, "every song the playlist took is still kept");

    rig.app.stop().await;
}

/// Liked Songs is read newest first, only to the first page holding a
/// song already taken, and its new likes go at the top in Spotify's order.
/// Its sync needs the scope that reads it.
#[tokio::test]
async fn liked_songs_take_new_likes_at_the_top_from_the_first_page() {
    let rig = rig("spotify-sync-liked").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, "playlist-read-private", SPOTIFY_TOKEN).await;
    let (status, body) = rig
        .call(
            &ada,
            "POST",
            "/api/v3/acquire/spotify/liked-songs/import",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let playlist_id = rig.import(&ada, "liked", SPOTIFY_LIKED_DEFAULT).await;
    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    assert_eq!(titles(&rig.app, &playlist_id).len(), SPOTIFY_LIKED_DEFAULT);

    let mut liked = vec![song(7), song(8)];
    liked.extend((0..SPOTIFY_LIKED_DEFAULT).map(|n| {
        spotify_track(
            &format!("sp-liked-{n}"),
            &format!("Liked {n}"),
            "Aurora Current",
            ("sp-album-liked", "Liked Album"),
            &format!("QZLIK26{n:05}"),
        )
    }));
    rig.spotify.set_liked(liked);
    let mark = rig.mark();
    rig.pass(&ada, "liked", &playlist_id).await;
    assert_eq!(
        rig.calls_since(mark),
        ["/me/tracks"],
        "one page holds a song already taken"
    );
    let titles = titles(&rig.app, &playlist_id);
    assert_eq!(titles.len(), SPOTIFY_LIKED_DEFAULT + 2);
    assert_eq!(
        titles[..3],
        ["Song 7", "Song 8", "Liked 0"],
        "new likes on top"
    );

    rig.link(&ada, "playlist-read-private", SPOTIFY_TOKEN).await;
    rig.switch(&ada, &playlist_id, false).await;
    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "SPOTIFY_RECONNECT_NEEDED");

    rig.app.stop().await;
}

/// Turning sync off cancels its queued pass. A link Spotify no longer
/// honours (the token refused, then the refresh refused) is recorded as
/// one closed reason, never Spotify's own words.
#[tokio::test]
async fn sync_off_cancels_and_a_refused_link_reads_not_linked() {
    let rig = rig("spotify-sync-refused").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify.set_playlist("sp-gone", "v1", vec![song(1)]);
    let playlist_id = rig.import(&ada, "sp-gone", 1).await;

    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    rig.link(&ada, SPOTIFY_SCOPES, "a-token-spotify-refuses")
        .await;
    rig.spotify.refuse_refresh(400, "invalid_grant");
    rig.pass(&ada, "sp-gone", &playlist_id).await;
    assert_eq!(
        rig.failure_and_version(&playlist_id).0.as_deref(),
        Some("not_linked")
    );
    let (status, body) = rig
        .call(
            &ada,
            "GET",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["failure"], "not_linked", "{body}");
    assert!(
        !body.to_string().contains("invalid_grant"),
        "no words of Spotify's: {body}"
    );

    rig.app
        .seed(format!(
            "UPDATE acquire_playlist_syncs SET next_check_at = 0 WHERE playlist_id = '{playlist_id}'"
        ))
        .await;
    rig.app
        .state()
        .acquire
        .spotify_sync
        .store()
        .enqueue_all(vec![playlist_id.clone()])
        .await
        .expect("the pass queues");
    let (status, body) = rig.switch(&ada, &playlist_id, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    let live: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'spotify.sync' AND idempotency_key = ?1 \
             AND state IN ('queued', 'running')",
            [format!("spotify:sync:{playlist_id}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, 0, "turning sync off cancels its pass");

    rig.app.stop().await;
}

/// The keys of the requests `who` filed (`track:` and a recording, or a
/// release group), sorted, with how many wait for an admin's approval.
fn requests_of(app: &TestApp, who: &Login) -> (Vec<String>, usize) {
    let rows: Vec<(String, String)> = app
        .db()
        .prepare(
            "SELECT musicbrainz_id_lower, status FROM request_history \
             WHERE user_id = ?1 ORDER BY musicbrainz_id_lower",
        )
        .unwrap()
        .query_map([&who.user_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let waiting = rows
        .iter()
        .filter(|(_, status)| status == "awaiting_approval")
        .count();
    (rows.into_iter().map(|(key, _)| key).collect(), waiting)
}

/// `songs` as `who`'s Spotify playlist `spotify_id`, imported and synced,
/// its first pass run.
async fn synced(rig: &Rig, who: &Login, spotify_id: &str, songs: Vec<Value>) -> String {
    let rows = songs.len();
    rig.spotify.set_playlist(spotify_id, "v1", songs);
    let playlist_id = rig.import(who, spotify_id, rows).await;
    let (status, body) = rig.switch(who, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    playlist_id
}

/// An admin's call.
async fn admin(rig: &Rig) -> Login {
    Login {
        user_id: String::new(),
        authorization: rig.app.login_admin().await,
    }
}

/// Approve, turn down or take back `playlist_id`'s requests as `admin`,
/// and wait for the pass an approval queues.
async fn decide(rig: &Rig, admin: &Login, playlist_id: &str, verb: &str) {
    let before = rig.checked_at(playlist_id);
    let (status, body) = rig
        .call(
            admin,
            "POST",
            &format!("/api/v3/requests/playlist-sync-approvals/{playlist_id}/{verb}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");
    if verb == "approve" {
        rig.app
            .until("the approved pass", || async {
                (rig.checked_at(playlist_id) != before).then_some(())
            })
            .await;
    }
}

async fn set_role(rig: &Rig, admin: &Login, who: &Login, role: &str) {
    let (status, body) = rig
        .call(
            admin,
            "PUT",
            &format!("/api/v3/admin/users/{}/role", who.user_id),
            Some(json!({"role": role})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The number on an admin's approvals badge.
async fn badge(rig: &Rig, admin: &Login) -> u64 {
    let (status, body) = rig
        .call(admin, "GET", "/api/v3/requests/approvals/count", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["count"].as_u64().expect("a count")
}

/// The sync as `who` reads it.
async fn sync_of(rig: &Rig, who: &Login, playlist_id: &str) -> Value {
    let (status, body) = rig
        .call(
            who,
            "GET",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// A plain user's synced playlist asks an admin once, and requests nothing
/// and asks MusicBrainz nothing until a yes. After the yes its songs are
/// requested by themselves, as songs that wait for no approval, 25 a pass
/// until it has caught up, from the ISRC reads the import made. Once the
/// yes is taken back new songs are still added and nothing is requested.
#[tokio::test]
async fn a_plain_users_sync_requests_only_under_its_grant() {
    let rig = rig("spotify-sync-grant").await;
    let admin = admin(&rig).await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let before_import = rig.musicbrainz.calls().len();
    let playlist_id = synced(&rig, &ada, "sp-grant", (1..=30).map(song).collect()).await;
    let after_import = rig.musicbrainz.calls().len();
    assert_eq!(
        after_import - before_import,
        30,
        "the import proves each album by one ISRC read"
    );
    assert_eq!(
        requests_of(&rig.app, &ada).0.len(),
        0,
        "nothing before a yes"
    );
    assert_eq!(sync_of(&rig, &ada, &playlist_id).await["grant"], "waiting");
    let (status, asks) = rig
        .call(
            &admin,
            "GET",
            "/api/v3/requests/playlist-sync-approvals",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{asks}");
    assert_eq!(asks["count"], 1, "{asks}");
    assert_eq!(asks["items"][0]["playlist_id"], playlist_id.as_str());
    assert_eq!(asks["items"][0]["song_count"], 30);
    assert_eq!(badge(&rig, &admin).await, 1, "the ask counts on the badge");

    decide(&rig, &admin, &playlist_id, "approve").await;
    assert_eq!(badge(&rig, &admin).await, 0, "and leaves it once approved");
    let (filed, waiting) = requests_of(&rig.app, &ada);
    assert_eq!(filed.len(), 25, "25 songs a pass: {filed:?}");
    assert_eq!(waiting, 0, "songs under the grant wait for no approval");
    assert!(
        filed.contains(&format!("track:{}", sync_recording(1))),
        "{filed:?}"
    );
    assert_eq!(
        rig.musicbrainz.calls().len(),
        after_import,
        "the walk reads the ISRCs the import read"
    );

    rig.pass(&ada, "sp-grant", &playlist_id).await;
    assert_eq!(
        requests_of(&rig.app, &ada).0.len(),
        30,
        "the rest next pass"
    );
    assert_eq!(
        sync_of(&rig, &ada, &playlist_id).await["progress"],
        "caught_up"
    );

    decide(&rig, &admin, &playlist_id, "revoke").await;
    rig.spotify
        .set_playlist("sp-grant", "v2", (1..=31).map(song).collect());
    rig.pass(&ada, "sp-grant", &playlist_id).await;
    assert_eq!(
        titles(&rig.app, &playlist_id).len(),
        31,
        "new songs still come"
    );
    assert_eq!(
        requests_of(&rig.app, &ada).0.len(),
        30,
        "nothing is requested"
    );
    assert_eq!(sync_of(&rig, &ada, &playlist_id).await["grant"], "revoked");

    rig.app.stop().await;
}

/// A trusted user's sync needs no grant, and asking for whole albums asks
/// once for an album two of its songs are on. Demoted to a plain user, the
/// owner's next pass asks an admin and requests nothing.
#[tokio::test]
async fn a_trusted_sync_asks_for_each_album_once() {
    let rig = rig("spotify-sync-albums").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let mut twin = song(901);
    twin["album"] = song(1)["album"].clone();
    rig.spotify
        .set_playlist("sp-albums", "v1", vec![song(1), twin, song(3)]);
    let playlist_id = rig.import(&tess, "sp-albums", 3).await;
    let (status, body) = rig
        .call(
            &tess,
            "PUT",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            Some(json!({"enabled": true, "whole_albums": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["grant"], "not_needed", "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    let asked = vec![sync_group(1), sync_group(3)];
    assert_eq!(
        requests_of(&rig.app, &tess),
        (asked.clone(), 0),
        "one ask an album"
    );

    set_role(&rig, &admin, &tess, "user").await;
    rig.spotify
        .set_playlist("sp-albums", "v2", vec![song(1), song(3), song(4)]);
    rig.pass(&tess, "sp-albums", &playlist_id).await;
    let grant: Option<String> = rig
        .app
        .db()
        .query_row(
            "SELECT grant_state FROM acquire_playlist_syncs WHERE playlist_id = ?1",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        grant.as_deref(),
        Some("pending"),
        "a demoted owner asks an admin"
    );
    assert_eq!(
        requests_of(&rig.app, &tess),
        (asked, 0),
        "and requests nothing"
    );

    rig.app.stop().await;
}

/// Songs a sync is about to ask for are not offered by Request missing:
/// the count leaves their albums out and the whole-playlist press leaves
/// them to the sync. A song the sync tried and could not ask for (its next
/// look put off) is offered again, and a press on one entry still asks
/// for it.
#[tokio::test]
async fn request_missing_leaves_the_syncs_songs_to_it() {
    let rig = rig("spotify-sync-banner").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-banner", "v1", (1..=30).map(song).collect());
    let playlist_id = rig.import(&tess, "sp-banner", 30).await;
    let (status, body) = rig
        .call(
            &tess,
            "PUT",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            Some(json!({"enabled": true, "whole_albums": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    assert_eq!(requests_of(&rig.app, &tess).0.len(), 25, "25 a pass");

    let state_path = format!("/api/v3/playlists/{playlist_id}/request-state");
    let state_of = || async {
        let (status, state) = rig.call(&tess, "GET", &state_path, None).await;
        assert_eq!(status, StatusCode::OK, "{state}");
        let queued: Vec<String> = state["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter(|entry| entry["state"] == "queued")
            .filter_map(|entry| entry["entry_id"].as_str().map(str::to_owned))
            .collect();
        (state["requestable_albums"].clone(), queued)
    };
    let (requestable, queued) = state_of().await;
    assert_eq!((requestable, queued.len()), (json!(0), 5));

    rig.app
        .seed(format!(
            "UPDATE catalog_playlist_source_songs SET looks = 1, look_again_at = 9000000000000 \
             WHERE playlist_id = '{playlist_id}' AND entry_id = '{}'",
            queued[4]
        ))
        .await;
    let (requestable, queued) = state_of().await;
    assert_eq!(
        (requestable, queued.len()),
        (json!(1), 4),
        "a song put off is offered again"
    );

    let press_path = format!("/api/v3/playlists/{playlist_id}/request-missing");
    let press = |entry_id: Option<&str>| {
        rig.call(
            &tess,
            "POST",
            &press_path,
            Some(json!({
                "idempotency_key": uuid::Uuid::new_v4().to_string(),
                "entry_id": entry_id,
            })),
        )
    };
    let (status, pressed) = press(None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pressed}");
    assert_eq!(pressed["requested"], 1, "only the song put off: {pressed}");
    let (status, pressed) = press(Some(&queued[0])).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pressed}");
    assert_eq!(pressed["requested"], 1, "{pressed}");

    rig.app.stop().await;
}

/// A full quota holds a sync's requests, and the songs are requested once
/// there is room. A request that ended is never asked again, even once the
/// request list forgets it.
#[tokio::test]
async fn a_full_quota_holds_and_an_ended_request_is_never_asked_again() {
    let rig = rig("spotify-sync-quota").await;
    let admin = admin(&rig).await;
    let quin = rig.app.login_user("quin").await;
    let quota = |count: u32| {
        let (rig, admin, quin) = (&rig, &admin, &quin);
        async move {
            let (status, body) = rig
                .call(
                    admin,
                    "PUT",
                    &format!("/api/v3/admin/users/{}/quota", quin.user_id),
                    Some(json!({"request_quota_count": count, "request_quota_days": 7})),
                )
                .await;
            assert!(status.is_success(), "{status} {body}");
        }
    };
    quota(1).await;
    rig.link(&quin, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let playlist_id = synced(&rig, &quin, "sp-quota", vec![song(4), song(5)]).await;
    decide(&rig, &admin, &playlist_id, "approve").await;
    let song_4 = format!("track:{}", sync_recording(4));
    assert_eq!(
        requests_of(&rig.app, &quin).0,
        std::slice::from_ref(&song_4)
    );
    assert_eq!(sync_of(&rig, &quin, &playlist_id).await["hold"], "quota");

    rig.app
        .seed(format!(
            "UPDATE request_history SET status = 'rejected' WHERE musicbrainz_id_lower = '{song_4}'; \
             DELETE FROM request_history WHERE musicbrainz_id_lower = '{song_4}'"
        ))
        .await;
    quota(10).await;
    rig.pass(&quin, "sp-quota", &playlist_id).await;
    assert_eq!(
        requests_of(&rig.app, &quin).0,
        [format!("track:{}", sync_recording(5))],
        "the held song is asked for, and the one followed up is never asked again"
    );
    assert_eq!(
        sync_of(&rig, &quin, &playlist_id).await["hold"],
        Value::Null
    );

    rig.app.stop().await;
}

/// When `playlist_id`'s song `song_id` is looked at next, and whether the
/// sync is done with it.
fn next_look_of(app: &TestApp, playlist_id: &str, song_id: &str) -> (Option<i64>, bool) {
    app.db()
        .query_row(
            "SELECT look_again_at, followed_up_at IS NOT NULL FROM catalog_playlist_source_songs \
             WHERE playlist_id = ?1 AND source_song_id = ?2",
            [playlist_id, song_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// A song the requests refuse for a reason the next pass would meet again
/// (here a title that names nothing) is looked at again later and stops
/// counting as left, rather than being tried every pass.
#[tokio::test]
async fn a_refused_song_waits_for_its_next_look() {
    let rig = rig("spotify-sync-refused").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let mut nameless = song(7);
    nameless["name"] = json!("Unknown");
    let playlist_id = synced(&rig, &tess, "sp-refused", vec![nameless]).await;

    assert_eq!(requests_of(&rig.app, &tess).0.len(), 0, "nothing filed");
    let (look, done) = next_look_of(&rig.app, &playlist_id, "sp-song-7");
    assert!(look.is_some() && !done, "looked at again later: {look:?}");
    let sync = sync_of(&rig, &tess, &playlist_id).await;
    assert_eq!(sync["songs_left"], 0, "{sync}");

    rig.app.stop().await;
}

/// A backlog of songs whose albums were asked for before costs a pass no
/// more MusicBrainz reads than its budget, even once the cached answers
/// are gone.
#[tokio::test]
async fn a_backlog_already_asked_for_keeps_to_the_pass_budget() {
    let rig = rig("spotify-sync-budget").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-budget", "v1", (1..=40).map(song).collect());
    let playlist_id = rig.import(&tess, "sp-budget", 40).await;
    let asked: Vec<String> = (1..=40)
        .map(|n| {
            format!(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, status) VALUES ('{group}', '{group}', \
                 'Aurora Current', 'Album {n}', 1, 'imported')",
                group = sync_group(n)
            )
        })
        .collect();
    rig.app.seed(asked.join("; ")).await;
    let (status, body) = rig
        .call(
            &admin,
            "POST",
            "/api/v3/admin/cache/clear",
            Some(json!({"scope": "provider_data"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let before = rig.musicbrainz.calls().len();
    let (status, body) = rig.switch(&tess, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    let reads = rig.musicbrainz.calls().len() - before;
    assert!(
        reads <= 25,
        "{reads} MusicBrainz reads in one pass, past the 25 a pass may make"
    );
    assert_eq!(
        requests_of(&rig.app, &tess).0.len(),
        0,
        "nothing asked again"
    );

    rig.app.stop().await;
}

/// Before an admin approves a plain user's sync, its new songs still get
/// their albums, by proof, and nothing is requested.
#[tokio::test]
async fn a_sync_waiting_for_its_grant_still_finds_albums() {
    let rig = rig("spotify-sync-waiting").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let playlist_id = synced(&rig, &ada, "sp-waiting", vec![song(1)]).await;
    rig.spotify
        .set_playlist("sp-waiting", "v2", vec![song(1), song(2)]);
    rig.pass(&ada, "sp-waiting", &playlist_id).await;

    let album: Option<String> = rig
        .app
        .db()
        .query_row(
            "SELECT album_id FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
             AND track_name = 'Song 2'",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(album, Some(sync_group(2)), "the new song has its album");
    assert_eq!(requests_of(&rig.app, &ada).0.len(), 0, "nothing requested");
    assert_eq!(sync_of(&rig, &ada, &playlist_id).await["grant"], "waiting");

    rig.app.stop().await;
}

/// While its grant waits, a pass reads only the songs that still need an
/// album: a song that has one is not visited at all, even one that plays
/// already, so the pass reads no library rows for it.
#[tokio::test]
async fn a_waiting_pass_visits_only_songs_without_an_album() {
    let rig = rig("spotify-sync-waiting-reads").await;
    let ada = rig.app.login_user("ada").await;
    rig.link(&ada, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-waiting-reads", "v1", (1..=60).map(song).collect());
    let playlist_id = rig.import(&ada, "sp-waiting-reads", 60).await;
    rig.app
        .seed(format!(
            "UPDATE catalog_playlist_tracks SET available_sources = '[\"jellyfin\"]' \
             WHERE playlist_id = '{playlist_id}' AND track_name = 'Song 60'"
        ))
        .await;
    let (status, body) = rig.switch(&ada, &playlist_id, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;

    assert_eq!(sync_of(&rig, &ada, &playlist_id).await["grant"], "waiting");
    let followed: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM catalog_playlist_source_songs \
             WHERE playlist_id = ?1 AND followed_up_at IS NOT NULL",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(followed, 0, "the song that plays waits for the grant too");

    rig.app.stop().await;
}

/// The recordings MusicBrainz finds for _Bohemian Rhapsody_'s ISRC,
/// recorded on 2026-11-24 and cut to six releases: two various-artists
/// compilations (one in an untyped group), _A Night at the Opera_ twice,
/// a 1975 single and a Queen compilation.
const BOHEMIAN: &str = include_str!("../fixtures/musicbrainz/sync_isrc_bohemian_rhapsody.json");
const NIGHT_AT_THE_OPERA: &str = "6b47c9a0-b9e1-3df9-a5e8-50a6ce0dbdbd";
const THE_1975_SINGLE: &str = "361a28b7-4588-44a6-8c96-86c1efb8258f";
const ON_THE_ALBUM: &str = "b1a9c0e9-d987-4042-ae91-78d6a3267d69";
const VARIOUS_ARTISTS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// The fixture's hits, changed by `edit` first.
fn hits(edit: impl FnOnce(&mut Value)) -> Vec<RecordingSearchHit> {
    let mut document: Value = serde_json::from_str(BOHEMIAN).expect("the fixture reads");
    edit(&mut document["recordings"]);
    serde_json::from_value(document["recordings"].take()).expect("the hits decode")
}

/// Spotify's filing of the song.
fn filed(title: &str, artist: &str, kind: &str) -> SpotifyAlbum {
    SpotifyAlbum {
        title: title.to_owned(),
        artist: artist.to_owned(),
        kind: Some(kind.to_owned()),
        total_tracks: Some(20),
        songs: Default::default(),
        isrcs: Default::default(),
    }
}

/// Each release of the fixture's second recording, the one on the album.
fn releases(recordings: &mut Value) -> &mut Vec<Value> {
    recordings[1]["releases"].as_array_mut().expect("releases")
}

/// A song Spotify files on a compilation is asked for on the artist's own
/// album when its ISRC's recording is there, and only then.
#[test]
fn a_compilation_song_moves_to_the_artists_own_album_only_on_proof() {
    let compilation = filed("Pop Classics", "Various Artists", "compilation");
    let moved = pick_own_group(&compilation, &hits(|_| {})).expect("moved");
    assert_eq!(moved.group, NIGHT_AT_THE_OPERA, "album beats single");
    assert_eq!(moved.title, "A Night at the Opera");
    assert_eq!(moved.recording.as_deref(), Some(ON_THE_ALBUM));

    let tied = hits(|recordings| {
        let mut twin = releases(recordings)[2].clone();
        twin["release-group"]["id"] = json!("a0000000-0000-4000-8000-000000000001");
        releases(recordings).push(twin);
    });
    assert_eq!(
        pick_own_group(&compilation, &tied),
        None,
        "two albums on one date refuse"
    );

    let coarse = hits(|recordings| {
        let mut twin = releases(recordings)[2].clone();
        twin["date"] = json!("1975");
        twin["release-group"]["id"] = json!("a0000000-0000-4000-8000-000000000003");
        releases(recordings).push(twin);
    });
    assert_eq!(
        pick_own_group(&compilation, &coarse),
        None,
        "a year and a full date in it may be one day: a tie"
    );

    let credited_elsewhere = hits(|recordings| {
        for release in releases(recordings).iter_mut() {
            if release["release-group"]["id"] == NIGHT_AT_THE_OPERA {
                release["artist-credit"][0]["artist"]["id"] = json!(VARIOUS_ARTISTS);
            }
        }
    });
    assert_eq!(
        pick_own_group(&compilation, &credited_elsewhere).map(|own| own.group),
        Some(THE_1975_SINGLE.to_owned()),
        "a group credited to another artist is not the artist's own"
    );

    let special = hits(|recordings| {
        for recording in recordings.as_array_mut().expect("recordings") {
            recording["artist-credit"][0]["artist"]["id"] = json!(VARIOUS_ARTISTS);
        }
    });
    assert_eq!(
        pick_own_group(&compilation, &special),
        None,
        "a special-purpose artist refuses"
    );

    let disagree = hits(|recordings| {
        recordings[0]["artist-credit"][0]["artist"]["id"] =
            json!("a0000000-0000-4000-8000-000000000002");
    });
    assert_eq!(
        pick_own_group(&compilation, &disagree),
        None,
        "artists that disagree refuse"
    );

    let soundtrack_only = hits(|recordings| {
        releases(recordings)
            .retain(|release| release["artist-credit"][0]["artist"]["id"] == VARIOUS_ARTISTS);
    });
    assert_eq!(
        pick_own_group(&compilation, &soundtrack_only),
        None,
        "a recording only on compilations stays as filed"
    );

    let album = filed("A Night at the Opera", "Queen", "album");
    assert_eq!(
        pick_own_group(&album, &hits(|_| {})),
        None,
        "an album Spotify files as the artist's own is left alone"
    );
}

/// _Bohemian Rhapsody_ as Spotify files it on a various-artists
/// compilation, with the ISRC the recorded MusicBrainz answer is for.
fn on_a_compilation() -> Value {
    json!({
        "id": "sp-bohemian", "type": "track", "name": "Bohemian Rhapsody",
        "artists": [{"name": "Queen"}],
        "album": {
            "id": "sp-pop-classics", "name": "Pop Classics", "images": [],
            "artists": [{"name": "Various Artists"}],
            "album_type": "compilation", "total_tracks": 24,
        },
        "external_ids": {"isrc": "GBUM71029604"},
        "track_number": 1, "disc_number": 1, "duration_ms": 354_000,
    })
}

/// The release group of the compilation Spotify files the song on.
const POP_CLASSICS: &str = "5679aa05-3efc-4bdf-ac5b-084800a5f14f";

/// The entry's album and name, and its source row's pick.
fn filing(app: &TestApp, playlist_id: &str) -> (Option<String>, String, Option<String>) {
    app.db()
        .query_row(
            "SELECT e.album_id, e.album_name, s.album_pick FROM catalog_playlist_tracks e \
             JOIN catalog_playlist_source_songs s ON s.entry_id = e.id WHERE e.playlist_id = ?1",
            [playlist_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

/// A song Spotify files on a compilation is asked for on the artist's own
/// album, and its row says where Spotify lists it. Its owner may keep it
/// on Spotify's album instead: the entry goes back there, the sync's
/// waiting request for the moved album is cancelled, the next pass asks
/// for the song on Spotify's album, and the rule never moves it again.
#[tokio::test]
async fn a_moved_song_can_be_kept_on_spotifys_album() {
    let rig = rig("spotify-sync-kept").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let playlist_id = synced(&rig, &tess, "sp-kept", vec![on_a_compilation()]).await;
    assert_eq!(
        filing(&rig.app, &playlist_id),
        (
            Some(NIGHT_AT_THE_OPERA.to_owned()),
            "A Night at the Opera".to_owned(),
            Some("own_album".to_owned())
        ),
        "asked for on the artist's own album"
    );
    let song = format!("track:{ON_THE_ALBUM}");
    assert_eq!(requests_of(&rig.app, &tess).0, std::slice::from_ref(&song));
    let (status, state) = rig
        .call(
            &tess,
            "GET",
            &format!("/api/v3/playlists/{playlist_id}/request-state"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{state}");
    assert_eq!(state["entries"][0]["listed_on"], "Pop Classics", "{state}");
    let entry: String = rig
        .app
        .db()
        .query_row(
            "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1",
            [&playlist_id],
            |row| row.get(0),
        )
        .unwrap();

    let bo = rig.app.login_user("bobby").await;
    let keep = format!("/api/v3/playlists/{playlist_id}/tracks/{entry}/spotify-album");
    let (status, _) = rig.call(&bo, "POST", &keep, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "only the owner keeps it");
    let before = rig.checked_at(&playlist_id);
    let status_of = || -> String {
        rig.app
            .db()
            .query_row(
                "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
                [&song],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(status_of(), "downloading", "the request has started");
    let (status, started) = rig.call(&tess, "POST", &keep, None).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(
        started["cancelled"], false,
        "a started request is left alone"
    );
    assert_eq!(status_of(), "downloading");
    rig.app
        .until("the pass the choice queues", || async {
            (rig.checked_at(&playlist_id) != before).then_some(())
        })
        .await;

    // Moved again, with its request still waiting for approval: that
    // request is cancelled, and the song asked for on Spotify's album.
    rig.app
        .seed(format!(
            "UPDATE request_history SET status = 'awaiting_approval' \
             WHERE musicbrainz_id_lower = '{song}'; \
             UPDATE catalog_playlist_tracks SET album_id = '{NIGHT_AT_THE_OPERA}', \
             album_name = 'A Night at the Opera', recording_mbid = '{ON_THE_ALBUM}' \
             WHERE playlist_id = '{playlist_id}'; \
             UPDATE catalog_playlist_source_songs SET album_pick = 'own_album', \
             filed_kind = 'track', filed_key = '{ON_THE_ALBUM}' \
             WHERE playlist_id = '{playlist_id}'"
        ))
        .await;
    let before = rig.checked_at(&playlist_id);
    let (status, kept) = rig.call(&tess, "POST", &keep, None).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["album_id"], POP_CLASSICS, "{kept}");
    assert_eq!(kept["cancelled"], true, "{kept}");
    rig.app
        .until("the pass the choice queues", || async {
            (rig.checked_at(&playlist_id) != before).then_some(())
        })
        .await;
    assert_eq!(
        filing(&rig.app, &playlist_id),
        (
            Some(POP_CLASSICS.to_owned()),
            "Pop Classics".to_owned(),
            Some("kept_spotify".to_owned())
        ),
        "the song stays on Spotify's album"
    );
    let asked_on: (String, Option<String>) = rig
        .app
        .db()
        .query_row(
            "SELECT status, track_release_group_mbid FROM request_history \
             WHERE musicbrainz_id_lower = ?1",
            [&song],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_ne!(asked_on.0, "cancelled", "asked for again");
    assert_eq!(
        asked_on.1.as_deref(),
        Some(POP_CLASSICS),
        "on Spotify's album"
    );

    let (status, body) = rig.call(&tess, "POST", &keep, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "ALBUM_NOT_MOVED");

    rig.app.stop().await;
}

/// `playlist_id`'s one entry, and the path that keeps it on Spotify's album.
fn keep_path(app: &TestApp, playlist_id: &str) -> String {
    let entry: String = app
        .db()
        .query_row(
            "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1",
            [playlist_id],
            |row| row.get(0),
        )
        .unwrap();
    format!("/api/v3/playlists/{playlist_id}/tracks/{entry}/spotify-album")
}

/// The status of the request for `key`, and who asked for it.
fn asked_by(app: &TestApp, key: &str) -> (String, Vec<String>) {
    let status: String = app
        .db()
        .query_row(
            "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap();
    let who = app
        .db()
        .prepare(
            "SELECT user_id FROM request_history_requesters \
             WHERE musicbrainz_id_lower = ?1 ORDER BY user_id",
        )
        .unwrap()
        .query_map([key], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    (status, who)
}

/// An admin keeping a moved song on Spotify's album takes back the sync's
/// ask for the moved album only while nobody else asked for it: another
/// person's ask for it stands.
#[tokio::test]
async fn keeping_spotifys_album_leaves_someone_elses_ask_standing() {
    let rig = rig("spotify-sync-kept-shared").await;
    let mut admin = admin(&rig).await;
    admin.user_id = rig
        .app
        .db()
        .query_row(
            "SELECT id FROM auth_users WHERE role = 'admin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    rig.link(&admin, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    rig.spotify
        .set_playlist("sp-shared", "v1", vec![on_a_compilation()]);
    let playlist_id = rig.import(&admin, "sp-shared", 1).await;
    let (status, body) = rig
        .call(
            &admin,
            "PUT",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            Some(json!({"enabled": true, "whole_albums": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.app
        .until("the first pass", || async {
            rig.checked_at(&playlist_id).map(|_| ())
        })
        .await;
    let bobby = rig.app.login_user("bobby").await;
    let (status, body) = rig
        .call(
            &bobby,
            "POST",
            "/api/v3/requests/albums",
            Some(json!({
                "musicbrainz_id": NIGHT_AT_THE_OPERA,
                "artist": "Queen",
                "album": "A Night at the Opera",
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    rig.app
        .seed(format!(
            "UPDATE request_history SET status = 'queued', download_task_id = NULL \
             WHERE musicbrainz_id_lower = '{NIGHT_AT_THE_OPERA}'"
        ))
        .await;
    assert_eq!(
        asked_by(&rig.app, NIGHT_AT_THE_OPERA).1.len(),
        2,
        "the sync and bobby both asked"
    );

    let (status, body) = rig
        .call(&admin, "POST", &keep_path(&rig.app, &playlist_id), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, who) = asked_by(&rig.app, NIGHT_AT_THE_OPERA);
    assert_eq!(status, "queued", "bobby's ask stands");
    assert!(who.contains(&bobby.user_id), "{who:?}");

    rig.app.stop().await;
}

/// Keeping a moved song takes back what the sync filed for it, even after
/// the owner turned Whole albums on: the song's own request. Turning Whole
/// albums on while sync is on starts a pass at once.
#[tokio::test]
async fn keeping_spotifys_album_takes_back_what_was_filed() {
    let rig = rig("spotify-sync-kept-filed").await;
    let admin = admin(&rig).await;
    let tess = rig.app.login_user("tess").await;
    set_role(&rig, &admin, &tess, "trusted").await;
    rig.link(&tess, SPOTIFY_SCOPES, SPOTIFY_TOKEN).await;
    let playlist_id = synced(&rig, &tess, "sp-filed", vec![on_a_compilation()]).await;
    let song = format!("track:{ON_THE_ALBUM}");
    assert_eq!(requests_of(&rig.app, &tess).0, std::slice::from_ref(&song));
    let passes = rig.passes(&playlist_id, "done");
    let (status, body) = rig
        .call(
            &tess,
            "PUT",
            &format!("/api/v3/playlists/{playlist_id}/sync"),
            Some(json!({"enabled": true, "whole_albums": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["whole_albums"], true, "{body}");
    rig.app
        .until("the pass Whole albums starts", || async {
            (rig.passes(&playlist_id, "done") > passes).then_some(())
        })
        .await;
    rig.app
        .seed(format!(
            "UPDATE request_history SET status = 'awaiting_approval', download_task_id = NULL \
             WHERE musicbrainz_id_lower = '{song}'"
        ))
        .await;

    let (status, body) = rig
        .call(&tess, "POST", &keep_path(&rig.app, &playlist_id), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cancelled"], true, "{body}");
    assert_eq!(asked_by(&rig.app, &song).0, "cancelled");

    rig.app.stop().await;
}
