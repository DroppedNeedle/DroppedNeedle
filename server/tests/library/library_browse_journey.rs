//! Library browse gaps and local file downloads on the production app:
//! membership, album status with upgrade tiers, top-songs track resolution,
//! the stats extras, track identity fields, the appearance sort, then track
//! and album downloads with the access setting, path safety, ranges and a
//! streamed ZIP read back, and local playback by track id.
//!
//! The catalog rows are seeded by hand, some over files that are missing,
//! unreadable or outside the folder. The app scans every new library
//! folder, so the folder is added empty and the files and rows go in after
//! that first scan: nothing scans it again while the journey runs.

use std::io::Read as _;

use axum::http::{HeaderMap, StatusCode, header};
use serde_json::{Value, json};

use crate::common::TestApp;

/// A real tagged FLAC, so the tag view has something to read.
fn opener() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/library/flac_full_01.flac"),
    )
    .expect("flac fixture")
}
const CLOSER: &[u8] = b"closer mp3 bytes, a little longer";

struct Fixture {
    app: TestApp,
    /// The owner's `authorization` header value.
    admin: String,
    /// A plain user's.
    user: String,
}

async fn fixture() -> Fixture {
    let app = TestApp::start("library-browse-journey").await;
    let admin = app.login_admin().await;
    let user = app.login_user("ada").await.authorization;
    // Upgrades are off until an admin allows them, and nothing is below
    // the cutoff while they are off.
    let headers = [("authorization", admin.as_str())];
    let policy = "/api/v3/settings/download-clients/policy";
    let (status, mut saved) = app.call("GET", policy, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved["upgrade_allowed"] = json!(true);
    let (status, saved) = app.call("PUT", policy, &headers, Some(saved)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["quality_cutoff"], "lossless");

    let dir = app.config.root_app_dir.clone();
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    app.add_library_folder("r1", &music).await;

    std::fs::create_dir_all(music.join("Aurora/First Light")).expect("album dir");
    std::fs::write(music.join("Aurora/First Light/01 Opener.flac"), opener()).expect("opener");
    std::fs::write(music.join("Aurora/First Light/02 Closer.mp3"), CLOSER).expect("closer");
    std::fs::write(dir.join("outside.flac"), b"not in the library").expect("outside");
    let seed = [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES \
         ('a1', 'Aurora', 'aurora', 'group', 1000, 1000), \
         ('a2', 'Boreal', 'boreal', 'person', 1500, 1500), \
         ('a3', 'Guest', 'guest', 'person', 1600, 1600)",
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'First Light', 'first light', 'Aurora', 'aurora', 'a1', 1994, 'automatic', 1000, 1000), \
         ('al2', 'r1', 'g2', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 'a2', 1999, 'automatic', 1500, 1500)",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         disc_number, track_number, year, duration_seconds, file_format, bit_rate, bit_depth, \
         channels, availability, ingest_source, imported_at, membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/01.flac', 'Aurora/First Light/01 Opener.flac', 'h1', 17, 1, 's1', \
          'Opener', 'opener', 'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', \
          1, 1, 1994, 200.0, 'flac', 900, 16, 2, 'indexed', 'scan', 1000.0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/02.mp3', 'Aurora/First Light/02 Closer.mp3', 'h2', 34, 1, 's2', \
          'Closer', 'closer', 'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', \
          1, 2, 1994, 180.0, 'mp3', 320, NULL, 2, 'indexed', 'scan', 1100.0, 'automatic'), \
         ('t3', 'al2', 'r1', '/m/03.flac', 'Boreal/gone.flac', 'h3', 5, 1, 's3', \
          'Peak', 'peak', 'Boreal', 'boreal', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', \
          1, 1, 1999, 220.0, 'flac', 900, 16, 2, 'indexed', 'scan', 1500.0, 'automatic'), \
         ('t4', 'al2', 'r1', '/m/04.flac', '../outside.flac', 'h4', 18, 1, 's4', \
          'Escape', 'escape', 'Guest', 'guest', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', \
          1, 2, 1999, 220.0, 'flac', 900, 16, 2, 'indexed', 'scan', 1550.0, 'automatic')",
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES \
         ('t1', 0, 'a1', 'main'), ('t2', 0, 'a1', 'main'), ('t3', 0, 'a2', 'main'), \
         ('t4', 0, 'a3', 'main')",
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES \
         ('al1', 0, 'a1', 'main'), ('al2', 0, 'a2', 'main')",
        "INSERT INTO local_album_external_identities \
         (local_album_id, provider, release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
         ('al1', 'musicbrainz', 'RG1', 'rel1', 'manual', 1000)",
        "INSERT INTO local_artist_external_identities \
         (local_artist_id, provider, provider_artist_id, decision_source, selected_at) VALUES \
         ('a1', 'musicbrainz', 'mb-aurora', 'manual', 1000)",
        "INSERT INTO local_track_external_identities \
         (local_track_id, provider, recording_mbid, decision_source, selected_at) VALUES \
         ('t1', 'musicbrainz', 'rec-opener', 'manual', 1000)",
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, candidates_json, \
         state, created_ms, updated_ms) VALUES \
         ('rv1', 'al2', 'low_confidence', '[]', 'pending', 1, 1), \
         ('rv2', 'al1', 'low_confidence', '[]', 'approved', 1, 1)",
        "INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope, \
         queued_at, updated_at, terminal_at) VALUES \
         ('run1', 'incremental', 'manual', 'completed', 'reconciling', '{}', 10, 20, 2000.5), \
         ('run2', 'incremental', 'manual', 'failed', 'reconciling', '{}', 30, 40, 9000000000.0)",
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, album_title, \
         requested_at, status) VALUES ('rg-req', 'RG-REQ', 'Someone', 'Wanted', 1704067200000, 'pending')",
    ]
    .join(";\n");
    app.db().execute_batch(&seed).expect("seed batch runs");
    Fixture { app, admin, user }
}

impl Fixture {
    /// One `/api/v3` request, signed by `who` when given.
    async fn send_ranged(
        &self,
        who: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
        range: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut headers = Vec::new();
        if let Some(who) = who {
            headers.push(("authorization", who));
        }
        if let Some(range) = range {
            headers.push(("range", range));
        }
        self.app
            .raw(method, &format!("/api/v3{path}"), &headers, body)
            .await
    }

    async fn send(
        &self,
        who: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        self.send_ranged(who, method, path, body, None).await
    }

    async fn json(
        &self,
        who: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, _, bytes) = self.send(Some(who), method, path, body).await;
        (
            status,
            serde_json::from_slice(&bytes).expect("body is json"),
        )
    }
}

#[tokio::test]
async fn browse_gaps_journey() {
    let fixture = fixture().await;

    // Membership: case-folded, owned by release group, requested by history.
    let (status, body) = fixture
        .json(
            &fixture.user,
            "POST",
            "/library/membership",
            Some(json!({"album_ids": ["RG1", " rg1 ", "rg-req", "rg-missing", ""]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"owned_ids": ["rg1"], "requested_ids": ["rg-req"]})
    );
    let too_many: Vec<String> = (0..501).map(|n| format!("id-{n}")).collect();
    let (status, body) = fixture
        .json(
            &fixture.user,
            "POST",
            "/library/membership",
            Some(json!({ "album_ids": too_many })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // The unfiltered form: every held release group and open request.
    let (status, body) = fixture
        .json(&fixture.user, "GET", "/library/mbids", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"mbids": ["rg1"], "requested_mbids": ["rg-req"]})
    );

    // Tags straight from the file: admins only, with v2's answers for a
    // file that is gone or unreadable.
    let (status, body) = fixture
        .json(&fixture.admin, "GET", "/library/tracks/t1/tags", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Airbag");
    assert_eq!(
        body["musicbrainz_release_group_id"],
        "b1392450-e666-3926-a536-22c65f834433"
    );
    let (status, body) = fixture
        .json(&fixture.admin, "GET", "/library/tracks/t2/tags", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["message"], "Could not read the audio file.");
    let (status, body) = fixture
        .json(&fixture.admin, "GET", "/library/tracks/t3/tags", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"]["message"],
        "The audio file is no longer present on disk."
    );
    let (status, _) = fixture
        .json(&fixture.admin, "GET", "/library/tracks/t4/tags", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = fixture
        .json(&fixture.admin, "GET", "/library/tracks/nope/tags", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = fixture
        .json(&fixture.user, "GET", "/library/tracks/t1/tags", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Album status by MusicBrainz id: tiers judged against the cutoff.
    let (status, body) = fixture
        .json(&fixture.user, "GET", "/library/albums/rg1/status", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["in_library"], true);
    assert_eq!(body["album_id"], "al1");
    assert_eq!(body["track_count"], 2);
    assert_eq!(body["tracks"][0]["id"], "t1");
    assert_eq!(body["tracks"][0]["current_tier"], "lossless");
    assert_eq!(body["tracks"][0]["below_cutoff"], false);
    assert_eq!(body["tracks"][1]["current_tier"], "mp3_320");
    assert_eq!(body["tracks"][1]["below_cutoff"], true);
    let (_, body) = fixture
        .json(&fixture.user, "GET", "/library/albums/al2/status", None)
        .await;
    assert_eq!(body["album_id"], "al2");
    let (_, body) = fixture
        .json(&fixture.user, "GET", "/library/albums/nope/status", None)
        .await;
    assert_eq!(
        body,
        json!({"in_library": false, "album_id": "nope", "track_count": 0, "tracks": []})
    );

    // Top-songs resolution: known positions resolve, the rest stay bare.
    let (status, body) = fixture
        .json(
            &fixture.user,
            "POST",
            "/library/resolve-tracks",
            Some(json!({"items": [
                {"release_group_mbid": "RG1", "track_number": 2},
                {"release_group_mbid": "rg1", "disc_number": 2, "track_number": 1},
                {"release_group_mbid": "rg-missing", "track_number": 1},
                {"release_group_mbid": "rg1"}
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["source"], "local");
    assert_eq!(body["items"][0]["track_source_id"], "t2");
    assert_eq!(body["items"][0]["stream_url"], "/api/v3/stream/local/t2");
    assert_eq!(body["items"][0]["format"], "mp3");
    for unresolved in 1..4 {
        assert_eq!(
            body["items"][unresolved]["source"],
            Value::Null,
            "item {unresolved}"
        );
    }

    // Stats extras: one pending review, one unidentified album, last
    // completed scan.
    let (_, body) = fixture
        .json(&fixture.user, "GET", "/library/stats", None)
        .await;
    assert_eq!(body["review_count"], 1);
    assert_eq!(body["local_only_count"], 1);
    let last_completed: f64 = fixture
        .app
        .db()
        .query_row(
            "SELECT MAX(terminal_at) FROM library_scan_runs WHERE state = 'completed'",
            [],
            |row| row.get(0),
        )
        .expect("scan runs read");
    // Read back through JSON text, as the answer was: parsing a float from
    // text can land one step off the value that was written.
    let as_sent: Value =
        serde_json::from_str(&json!(last_completed).to_string()).expect("number parses");
    assert_eq!(body["last_scan_at"], as_sent);

    // Track identity and quality fields.
    let (_, body) = fixture
        .json(&fixture.user, "GET", "/library/tracks/t1", None)
        .await;
    assert_eq!(body["recording_mbid"], "rec-opener");
    assert_eq!(body["release_group_mbid"], "RG1");
    assert_eq!(body["artist_mbid"], "mb-aurora");
    assert_eq!(body["album_artist_mbid"], "mb-aurora");
    assert_eq!(body["album_artist_id"], "a1");
    assert_eq!(body["bit_depth"], 16);
    assert_eq!(body["channels"], 2);

    // Appearance sort: the guest credited on someone else's album leads.
    let (status, body) = fixture
        .json(
            &fixture.user,
            "GET",
            "/library/artists?sort=appearance_count&order=desc",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["id"], "a3");
    assert_eq!(body["items"][0]["appearance_album_count"], 1);
    fixture.app.stop().await;
}

fn zip_entries(bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip parses");
    (0..archive.len())
        .map(|index| {
            let mut file = archive.by_index(index).expect("entry opens");
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .expect("entry reads, crc checked");
            (file.name().expect("entry name").into_owned(), data)
        })
        .collect()
}

#[tokio::test]
async fn local_downloads_journey() {
    let fixture = fixture().await;

    let (status, body) = fixture
        .json(&fixture.user, "GET", "/download/access", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"allowed": true}));

    // One track, byte for byte, as an attachment.
    let (status, headers, bytes) = fixture
        .send(Some(&fixture.user), "GET", "/download/local/track/t1", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, opener());
    assert_eq!(headers[header::CONTENT_TYPE], "audio/flac");
    assert_eq!(
        headers[header::CONTENT_LENGTH],
        opener().len().to_string().as_str()
    );
    let disposition = headers[header::CONTENT_DISPOSITION]
        .to_str()
        .expect("ascii header");
    assert!(
        disposition.starts_with("attachment; filename=\"01 Opener.flac\""),
        "{disposition}"
    );

    // A single range resumes a track download; one past the end is 416.
    let (status, headers, bytes) = fixture
        .send_ranged(
            Some(&fixture.user),
            "GET",
            "/download/local/track/t2",
            None,
            Some("bytes=7-11"),
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, &CLOSER[7..12]);
    assert_eq!(
        headers[header::CONTENT_RANGE],
        format!("bytes 7-11/{}", CLOSER.len()).as_str()
    );
    assert!(headers.contains_key(header::CONTENT_DISPOSITION));
    let (status, headers, _) = fixture
        .send_ranged(
            Some(&fixture.user),
            "GET",
            "/download/local/track/t2",
            None,
            Some("bytes=999-"),
        )
        .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        headers[header::CONTENT_RANGE],
        format!("bytes */{}", CLOSER.len()).as_str()
    );

    // Playback resolves the same track ids through the same catalog
    // lookup and root confinement.
    let (status, headers, bytes) = fixture
        .send(Some(&fixture.user), "GET", "/stream/local/t1", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, opener());
    assert_eq!(headers[header::CONTENT_TYPE], "audio/flac");
    let (status, _, _) = fixture
        .send(Some(&fixture.user), "GET", "/stream/local/t4", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = fixture
        .send(Some(&fixture.user), "GET", "/stream/local/nope", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Path safety: a row pointing outside the root is refused, a file gone
    // from disk and an unknown id are 404.
    let (status, _, _) = fixture
        .send(Some(&fixture.user), "GET", "/download/local/track/t4", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = fixture
        .send(Some(&fixture.user), "GET", "/download/local/track/t3", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fixture
        .send(
            Some(&fixture.user),
            "GET",
            "/download/local/track/nope",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The album as a streamed ZIP, by local id and by MusicBrainz id.
    let expected = vec![
        ("01 Opener.flac".to_owned(), opener()),
        ("02 Closer.mp3".to_owned(), CLOSER.to_vec()),
    ];
    for path in [
        "/download/local/album/al1",
        "/download/local/album/mbid/rg1",
    ] {
        let (status, headers, bytes) = fixture.send(Some(&fixture.user), "GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
        assert_eq!(
            headers[header::CONTENT_LENGTH],
            bytes.len().to_string().as_str(),
            "{path}: the advertised length is exact"
        );
        let disposition = headers[header::CONTENT_DISPOSITION]
            .to_str()
            .expect("ascii");
        assert!(
            disposition.contains("Aurora - First Light.zip"),
            "{disposition}"
        );
        assert_eq!(zip_entries(bytes), expected, "{path}");
    }
    // An album with nothing servable on disk is 404, as is an unknown mbid.
    let (status, _, _) = fixture
        .send(
            Some(&fixture.user),
            "GET",
            "/download/local/album/al2",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fixture
        .send(
            Some(&fixture.user),
            "GET",
            "/download/local/album/mbid/rg-x",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The access setting is read per request: admins-only refuses the user
    // and still admits the admin.
    let (status, mut security) = fixture
        .json(&fixture.admin, "GET", "/settings/security", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{security}");
    security["library_download_access"] = json!("admin");
    let (status, saved) = fixture
        .json(&fixture.admin, "PUT", "/settings/security", Some(security))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, body) = fixture
        .json(&fixture.user, "GET", "/download/access", None)
        .await;
    assert_eq!(body, json!({"allowed": false}));
    let (status, body) = fixture
        .json(&fixture.user, "GET", "/download/local/album/al1", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["error"]["message"],
        "Library downloads are restricted by the administrator"
    );
    let (status, _, _) = fixture
        .send(
            Some(&fixture.admin),
            "GET",
            "/download/local/track/t2",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // No session, no download.
    let (status, headers, _) = fixture.send(None, "GET", "/download/access", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    fixture.app.stop().await;
}
