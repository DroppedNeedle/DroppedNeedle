//! The Duplicates page on the production app: every group two albums own
//! is listed with only the albums that own it, a guess its files contradict
//! counts only under the group its tags name, a copy's release counts only
//! under its own group, a copy with fewer tracks marks the group, and the
//! list pages by cursor. The page is a curator's.

use axum::http::StatusCode;
use serde_json::Value;

use crate::common::TestApp;

const G1: &str = "0a0b0c0d-0001-4000-8000-000000000001";
const G2: &str = "0a0b0c0d-0001-4000-8000-000000000002";
const G3: &str = "0a0b0c0d-0001-4000-8000-000000000003";
const R1: &str = "0a0b0c0d-0002-4000-8000-000000000001";
const R3: &str = "0a0b0c0d-0002-4000-8000-000000000003";

/// Album `id` with `tracks` indexed files whose tags name `tag`, its match
/// row `(group, release)` (`release` may be absent) and its match flag reason.
fn album(
    id: &str,
    created_at: u32,
    tracks: u32,
    tag: Option<&str>,
    row: Option<(&str, Option<&str>)>,
    flag: Option<&str>,
) -> String {
    let mut sql = format!(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, grouping_source, \
         created_at, updated_at) VALUES ('{id}', 'r1', '{id}', 'Title {id}', 'title {id}', \
         'Aurora', 'aurora', 'a1', 1994, 'automatic', {created_at}, {created_at});"
    );
    let tag = tag.map_or("NULL".to_owned(), |tag| format!("'{tag}'"));
    for n in 1..=tracks {
        sql.push_str(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, \
             album_artist_name, album_artist_name_folded, disc_number, track_number, year, \
             duration_seconds, file_format, availability, ingest_source, imported_at, \
             membership_source, embedded_release_group_mbid) VALUES ('{id}-{n}', '{id}', 'r1', \
             '/m/{id}-{n}.flac', '{id}/{n}.flac', 'h-{id}-{n}', 1000, 1, 's', 'Song {n}', \
             'song {n}', 'Aurora', 'aurora', 'Title {id}', 'title {id}', 'Aurora', 'aurora', \
             1, {n}, 1994, 200.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic', {tag});"
        ));
    }
    if let Some((group, release)) = row {
        let release = release.map_or("NULL".to_owned(), |release| format!("'{release}'"));
        sql.push_str(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{id}', 'musicbrainz', '{group}', {release}, 'automatic', 1000);"
        ));
    }
    if let Some(reason) = flag {
        sql.push_str(&format!(
            "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             candidates_json, updated_at) VALUES ('{id}', 'unconfirmed', '{reason}', '[]', 1000);"
        ));
    }
    sql
}

fn copy_ids(group: &Value) -> Vec<&str> {
    group["copies"]
        .as_array()
        .expect("copies")
        .iter()
        .map(|copy| copy["album_id"].as_str().expect("album id"))
        .collect()
}

#[tokio::test]
async fn duplicates_list_every_owned_group_and_page_by_cursor() {
    let app = TestApp::start("library-duplicates").await;
    let admin = app.login_admin().await;
    let user = app.login_user("ada").await.authorization;
    let music = app.config.root_app_dir.join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    app.add_library_folder("r1", &music).await;

    // G1: two copies of one release, one with fewer tracks. G2: an
    // unmatched album its tags name, and a guess for G1 its tags contradict.
    // G3: two copies with as many tracks, one matched to no release.
    let seed = "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                updated_at) VALUES ('a1', 'Aurora', 'aurora', 'group', 1000, 1000);"
        .to_owned()
        + &album("al1", 1000, 3, None, Some((G1, Some(R1))), None)
        + &album("al2", 1100, 2, None, Some((G1, Some(R1))), None)
        + &album(
            "al3",
            1200,
            1,
            Some(G2),
            Some((G1, Some(R3))),
            Some("NAMED_GROUP_DIFFERS"),
        )
        + &album("al4", 1300, 2, Some(G2), None, None)
        + &album("al5", 1400, 2, None, Some((G3, Some(R1))), None)
        + &album("al6", 1500, 2, None, Some((G3, None)), None);
    app.seed(&seed).await;

    let path = "/api/v3/library/duplicates?limit=1";
    let headers = [("authorization", admin.as_str())];
    let (status, first) = app.call("GET", path, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let items = first["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "{first}");
    let g1 = &items[0];
    assert_eq!(g1["id"], G1);
    assert_eq!(
        copy_ids(g1),
        ["al1", "al2"],
        "the doubted guess is not a copy"
    );
    assert_eq!(
        g1["classes"],
        serde_json::json!(["same_release", "incomplete_copy"]),
        "{g1}"
    );
    assert_eq!(g1["copies"][1]["track_count"], 2);

    let cursor = first["next_cursor"].as_str().expect("a second page");
    let (status, second) = app
        .call("GET", &format!("{path}&cursor={cursor}"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let g2 = &second["items"][0];
    assert_eq!(g2["id"], G2);
    assert_eq!(copy_ids(g2), ["al3", "al4"]);
    assert_eq!(
        g2["classes"],
        serde_json::json!(["incomplete_copy"]),
        "{g2}"
    );
    let guess = &g2["copies"][0];
    assert_eq!(guess["doubted"], true);
    assert_eq!(guess["evidence"], "file_tags");
    assert_eq!(
        guess["release_mbid"],
        Value::Null,
        "a release of another group is not shown"
    );

    let cursor = second["next_cursor"].as_str().expect("a third page");
    let (status, third) = app
        .call("GET", &format!("{path}&cursor={cursor}"), &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{third}");
    assert_eq!(third["next_cursor"], Value::Null, "{third}");
    let g3 = &third["items"][0];
    assert_eq!(g3["id"], G3, "a group two albums own is listed: {third}");
    assert_eq!(copy_ids(g3), ["al5", "al6"]);
    assert_eq!(g3["classes"], serde_json::json!([]), "{g3}");

    let (status, _) = app
        .call("GET", path, &[("authorization", user.as_str())], None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a plain user is refused");
}
