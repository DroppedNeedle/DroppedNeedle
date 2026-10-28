//! Two users of the same role, and what each may do to what the other owns.
//!
//! - Path-id rows: user A owns the resource the path names. User B gets
//!   the answer pinned here (403 or 404) and A's resource is unchanged
//!   after B's attempts. An admin gets the answer pinned in the admin
//!   column, on a resource of its own seeding so a destructive admin call
//!   cannot muddy the check. A, the owner, is admitted.
//! - Caller-keyed rows take no other user's id: A writes, B's read shows
//!   none of it, and B's write leaves A's alone. The list is a sample:
//!   playback start, progress and stop, scrobbling, the `/me/*`
//!   preferences and discover's ignore list are caller-keyed the same way.
//! - Body-id rows take a user-owned id in the body. They join by hand: a
//!   new route that takes one adds a row to `BODY_ID_OPERATIONS` and a
//!   check to `body_ids_never_reach_another_users_resources`.
//!
//! The coverage check holds every user and curator operation with a path
//! parameter to a row here or an exemption with its reason, so a new
//! user-owned route fails until it joins. The role matrix (401 and roles)
//! is `auth_e2e::auth_on_every_endpoint`.

use std::collections::HashMap;

use axum::http::StatusCode;
use droppedneedle::docs::ApiDoc;
use droppedneedle_acquire::downloads::{
    state::TaskStatus,
    store::{NewTask, TaskOutcome},
};
use droppedneedle_acquire::held::store::{
    EntryOrigin, GroupKey, HeldRoot, HeldRows, NewEntry, NewFile,
};
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::requests::ledger::{RequestStatus, WantedWatch, WatchState};
use droppedneedle_acquire::requests::models::RequestKind;
use droppedneedle_acquire::search_jobs::store::{COMPLETED, JobPayload, JobStore, NewJob};
use serde_json::{Value, json};
use utoipa::OpenApi as _;

use crate::common::TestApp;

/// A signed-in account.
struct Who {
    id: String,
    username: String,
    auth: String,
}

/// One request as `who`. The limiter is emptied first: the table makes
/// far more calls per user than a person would.
async fn call(
    app: &TestApp,
    who: &Who,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    app.state().auth.limits.reset();
    let (status, _, bytes) = app
        .raw(method, uri, &[("authorization", who.auth.as_str())], body)
        .await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn user(app: &TestApp, username: &str) -> Who {
    let login = app.login_user(username).await;
    Who {
        id: login.user_id,
        username: username.to_owned(),
        auth: login.authorization,
    }
}

async fn trusted(app: &TestApp, admin: &Who, username: &str) -> Who {
    let who = user(app, username).await;
    let (status, body) = call(
        app,
        admin,
        "PUT",
        &format!("/api/v3/admin/users/{}/role", who.id),
        Some(json!({"role": "trusted"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    who
}

/// A resource one user owns, as the path-id rows name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owned {
    Playlist,
    PublicPlaylist,
    Batch,
    AppPassword,
    Session,
    Avatar,
    Request,
    Wanted,
    Task,
    Held,
    SearchJob,
    SpotifyJob,
    /// Drop-import jobs and items, owned by one of two trusted users.
    Drop,
    /// An edition's tag write, a management job one of two trusted users
    /// asked for.
    TagWrite,
    /// A request still waiting, which a cancel can reach.
    LiveRequest,
    /// A failed track request.
    TrackRequest,
    /// A held entry of one of two trusted users, which only a curator may
    /// match to an album.
    HeldMatch,
}

const ADMITTED: u16 = 0;

/// Path-id rows: the resource, the operation, B's answer, the admin's
/// answer. `ADMITTED` means anything but 401, 403, 404 or a 5xx. Within a
/// resource, destructive operations come last, since the owner pass runs
/// them in order on one resource.
const PATH_ID_ROWS: &[(Owned, &str, &str, u16, u16)] = &[
    // Playlists: a private one is invisible to everyone else, admins
    // included: admins never see another user's private playlist.
    (
        Owned::Playlist,
        "GET",
        "/api/v3/playlists/{playlist_id}",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "GET",
        "/api/v3/playlists/{playlist_id}/cover",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "PUT",
        "/api/v3/playlists/{playlist_id}",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "POST",
        "/api/v3/playlists/{playlist_id}/cover",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "POST",
        "/api/v3/playlists/{playlist_id}/resolve-sources",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/reorder",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/visibility",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks/remove",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}/cover",
        404,
        404,
    ),
    (
        Owned::Playlist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}",
        404,
        404,
    ),
    // A public playlist: anyone reads it, only its owner changes it.
    (
        Owned::PublicPlaylist,
        "GET",
        "/api/v3/playlists/{playlist_id}",
        200,
        200,
    ),
    (
        Owned::PublicPlaylist,
        "GET",
        "/api/v3/playlists/{playlist_id}/cover",
        200,
        200,
    ),
    (
        Owned::PublicPlaylist,
        "PUT",
        "/api/v3/playlists/{playlist_id}",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "POST",
        "/api/v3/playlists/{playlist_id}/cover",
        403,
        403,
    ),
    // A read: the sources of each entry.
    (
        Owned::PublicPlaylist,
        "POST",
        "/api/v3/playlists/{playlist_id}/resolve-sources",
        200,
        200,
    ),
    (
        Owned::PublicPlaylist,
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/reorder",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "PATCH",
        "/api/v3/playlists/{playlist_id}/visibility",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks/remove",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}/cover",
        403,
        403,
    ),
    (
        Owned::PublicPlaylist,
        "DELETE",
        "/api/v3/playlists/{playlist_id}",
        403,
        403,
    ),
    // Discovery batches: owner or admin.
    (
        Owned::Batch,
        "GET",
        "/api/v3/discover/batches/{batch_id}",
        404,
        ADMITTED,
    ),
    (
        Owned::Batch,
        "DELETE",
        "/api/v3/discover/batches/{batch_id}",
        404,
        ADMITTED,
    ),
    // App passwords and sessions under the caller's own `/me` and
    // `/auth`: an admin's oversight is under `/admin`.
    (
        Owned::AppPassword,
        "DELETE",
        "/api/v3/me/app-passwords/{id}",
        404,
        404,
    ),
    (
        Owned::Session,
        "DELETE",
        "/api/v3/auth/sessions/{id}",
        404,
        404,
    ),
    // Avatars: self or admin.
    (
        Owned::Avatar,
        "GET",
        "/api/v3/users/{id}/avatar",
        403,
        ADMITTED,
    ),
    // Requests (A's has failed): a requester or an admin.
    (
        Owned::Request,
        "POST",
        "/api/v3/requests/retry/{musicbrainz_id}",
        403,
        ADMITTED,
    ),
    (
        Owned::Request,
        "DELETE",
        "/api/v3/requests/history/{musicbrainz_id}",
        403,
        ADMITTED,
    ),
    (
        Owned::TrackRequest,
        "POST",
        "/api/v3/requests/tracks/{musicbrainz_id}/from-album",
        403,
        ADMITTED,
    ),
    (
        Owned::LiveRequest,
        "DELETE",
        "/api/v3/requests/active/{musicbrainz_id}",
        403,
        ADMITTED,
    ),
    (
        Owned::Wanted,
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/seen",
        404,
        ADMITTED,
    ),
    (
        Owned::Wanted,
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/resume",
        404,
        ADMITTED,
    ),
    (
        Owned::Wanted,
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/stop",
        404,
        ADMITTED,
    ),
    // Download tasks: owner or admin.
    (
        Owned::Task,
        "GET",
        "/api/v3/downloads/tasks/{task_id}",
        403,
        ADMITTED,
    ),
    (
        Owned::Task,
        "GET",
        "/api/v3/downloads/tasks/{task_id}/files",
        403,
        ADMITTED,
    ),
    (
        Owned::Task,
        "POST",
        "/api/v3/downloads/tasks/{task_id}/next-source",
        403,
        ADMITTED,
    ),
    (
        Owned::Task,
        "POST",
        "/api/v3/downloads/tasks/{task_id}/retry",
        403,
        ADMITTED,
    ),
    (
        Owned::Task,
        "POST",
        "/api/v3/downloads/tasks/{task_id}/cancel",
        403,
        ADMITTED,
    ),
    // Held entries: owner or admin.
    (Owned::Held, "GET", "/api/v3/held/{entry_id}", 404, ADMITTED),
    (
        Owned::Held,
        "GET",
        "/api/v3/held/files/{file_id}/audio",
        404,
        ADMITTED,
    ),
    (
        Owned::Held,
        "POST",
        "/api/v3/held/{entry_id}/recheck",
        404,
        ADMITTED,
    ),
    (
        Owned::Held,
        "POST",
        "/api/v3/held/{entry_id}/import",
        404,
        ADMITTED,
    ),
    (
        Owned::Held,
        "POST",
        "/api/v3/held/{entry_id}/discard",
        404,
        ADMITTED,
    ),
    // Matching a held entry, between two trusted users: owner or admin.
    (
        Owned::HeldMatch,
        "POST",
        "/api/v3/held/{entry_id}/match",
        404,
        ADMITTED,
    ),
    // Search jobs: the owner; an admin may only dismiss another user's.
    (
        Owned::SearchJob,
        "GET",
        "/api/v3/downloads/search/{job_id}",
        403,
        403,
    ),
    (
        Owned::SearchJob,
        "POST",
        "/api/v3/downloads/search/{job_id}/pick",
        403,
        403,
    ),
    (
        Owned::SearchJob,
        "POST",
        "/api/v3/downloads/search/{job_id}/dismiss",
        403,
        ADMITTED,
    ),
    (
        Owned::SearchJob,
        "POST",
        "/api/v3/downloads/search/{job_id}/cancel",
        403,
        403,
    ),
    // Spotify import jobs are keyed by their user: nobody else finds one.
    (
        Owned::SpotifyJob,
        "GET",
        "/api/v3/acquire/spotify/jobs/{id}",
        404,
        404,
    ),
    // Drop imports, between two trusted users: owner or admin.
    (
        Owned::Drop,
        "GET",
        "/api/v3/import/jobs/{job_id}",
        404,
        ADMITTED,
    ),
    // An edition's tag write, between two trusted users: writer or admin.
    (
        Owned::TagWrite,
        "GET",
        "/api/v3/library/albums/{album_id}/edition/tags/{job_id}",
        404,
        ADMITTED,
    ),
    (
        Owned::TagWrite,
        "POST",
        "/api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply",
        404,
        ADMITTED,
    ),
    (
        Owned::TagWrite,
        "POST",
        "/api/v3/library/albums/{album_id}/edition/tags/{job_id}/undo",
        404,
        ADMITTED,
    ),
];

/// Caller-keyed operations with a path parameter that names no user: each
/// is driven by `caller_keyed_state_stays_with_its_caller`.
const CALLER_KEYED_OPERATIONS: &[(&str, &str)] = &[
    ("PUT", "/api/v3/favorites/{kind}/{item_id}"),
    ("PUT", "/api/v3/ratings/{kind}/{item_id}"),
    ("PUT", "/api/v3/artists/{artist_mbid}/follow"),
    ("PUT", "/api/v3/artists/{artist_mbid}/auto-download"),
    ("GET", "/api/v3/remotes/{source}/connection"),
    ("PUT", "/api/v3/remotes/{source}/connection"),
    ("DELETE", "/api/v3/remotes/{source}/connection"),
];

/// Operations that take a user-owned id in the body, each driven by
/// `body_ids_never_reach_another_users_resources`. Joined by hand.
const BODY_ID_OPERATIONS: &[(&str, &str)] = &[
    ("POST", "/api/v3/requests/batches/cancel"),
    ("POST", "/api/v3/held/recheck"),
    ("POST", "/api/v3/discover/playlist-suggestions"),
];

/// Resource families whose reads (GET and HEAD) hold nothing one user
/// owns. Every other operation under them is a row or an exact exemption.
const EXEMPT_READS: &[(&str, &str)] = &[
    ("/api/v3/albums/", "shared catalog albums"),
    ("/api/v3/artists/", "shared catalog artists"),
    ("/api/v3/covers/", "shared cover art"),
    (
        "/api/v3/discover/queue/",
        "catalog release groups behind the discover deck",
    ),
    (
        "/api/v3/download/local/",
        "library files, shared by everyone",
    ),
    ("/api/v3/home/genre/", "shared catalog genres"),
    ("/api/v3/library/albums/", "shared library albums"),
    ("/api/v3/library/artists/", "shared library artists"),
    (
        "/api/v3/library/contributions/",
        "contributions, shared by curators",
    ),
    ("/api/v3/library/genres/", "shared library genres"),
    (
        "/api/v3/library/reviews/",
        "library reviews, shared by curators",
    ),
    ("/api/v3/library/scan/runs/", "library scans, shared"),
    ("/api/v3/library/tracks/", "shared library tracks"),
    (
        "/api/v3/plugins/ext/",
        "a plugin's own routes: the plugin checks its caller",
    ),
    (
        "/api/v3/remotes/{source}/",
        "the caller's own remote server account",
    ),
    ("/api/v3/search/", "shared catalog search"),
    (
        "/api/v3/stream/",
        "library audio or the caller's own remote server",
    ),
    (
        "/api/v3/youtube/",
        "YouTube links, shared by everyone on the server",
    ),
];

const SHARED_ALBUM: &str = "acts on a shared library album; curators act on any";
const SHARED_CONTRIBUTION: &str =
    "contributions are shared by curators: the actor is recorded, not checked";
const SHARED_REVIEW: &str = "library reviews are shared by curators";
const SHARED_LINK: &str = "YouTube links are shared by everyone on the server";
const PLUGIN_ROUTE: &str = "a plugin's own route: the plugin checks its caller";

/// Writes under an exempt family that hold nothing one user owns.
const EXACT_EXEMPTIONS: &[(&str, &str, &str)] = &[
    (
        "POST",
        "/api/v3/albums/{album_id}/edition/acquire",
        "acquires a shared catalog album for the library",
    ),
    (
        "POST",
        "/api/v3/albums/{album_id}/refresh",
        "refreshes shared catalog metadata",
    ),
    (
        "POST",
        "/api/v3/discover/queue/preview/{release_group_mbid}",
        "looks up a preview for a catalog release group",
    ),
    (
        "PUT",
        "/api/v3/library/albums/{album_id}/edition",
        SHARED_ALBUM,
    ),
    (
        "DELETE",
        "/api/v3/library/albums/{album_id}/edition",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{album_id}/edition/confirm",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{album_id}/edition/extras/remove",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{album_id}/removals/{removal_id}/put-back",
        SHARED_ALBUM,
    ),
    (
        "PUT",
        "/api/v3/library/albums/{album_id}/edition/files/{track_id}",
        SHARED_ALBUM,
    ),
    (
        "DELETE",
        "/api/v3/library/albums/{album_id}/edition/files/{track_id}",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{album_id}/edition/undo",
        SHARED_ALBUM,
    ),
    ("POST", "/api/v3/library/folds/{fold_id}/undo", SHARED_ALBUM),
    (
        "POST",
        "/api/v3/library/artist-repairs/{repair_id}/undo",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{album_id}/edition/tags",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/albums/{id}/contributions",
        SHARED_ALBUM,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/cancel",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/remove",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/search",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/select",
        SHARED_CONTRIBUTION,
    ),
    (
        "PUT",
        "/api/v3/library/contributions/{id}/draft",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/attach",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/duplicates",
        SHARED_CONTRIBUTION,
    ),
    (
        "PUT",
        "/api/v3/library/contributions/{id}/musicbrainz/result",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/seed",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/verify",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/rebuild",
        SHARED_CONTRIBUTION,
    ),
    (
        "POST",
        "/api/v3/library/reviews/{id}/approve",
        SHARED_REVIEW,
    ),
    ("POST", "/api/v3/library/reviews/{id}/detach", SHARED_REVIEW),
    (
        "POST",
        "/api/v3/library/reviews/{id}/dismiss",
        SHARED_REVIEW,
    ),
    ("POST", "/api/v3/library/reviews/{id}/reject", SHARED_REVIEW),
    (
        "DELETE",
        "/api/v3/library/tracks/{id}",
        "removes a shared library track",
    ),
    ("POST", "/api/v3/plugins/ext/{name}/{subpath}", PLUGIN_ROUTE),
    (
        "DELETE",
        "/api/v3/plugins/ext/{name}/{subpath}",
        PLUGIN_ROUTE,
    ),
    (
        "POST",
        "/api/v3/remotes/{source}/playlists/{id}/import",
        "imports from the caller's own remote account into a playlist of their own",
    ),
    (
        "POST",
        "/api/v3/acquire/spotify/playlists/{id}/import",
        "imports from the caller's own Spotify account into a playlist of their own",
    ),
    ("PUT", "/api/v3/youtube/link/{album_id}", SHARED_LINK),
    ("DELETE", "/api/v3/youtube/link/{album_id}", SHARED_LINK),
    (
        "DELETE",
        "/api/v3/youtube/track-link/{album_id}/{disc_number}/{track_number}",
        SHARED_LINK,
    ),
];

/// The document's user and curator operations, as (method, path).
fn gated_operations() -> Vec<(String, String)> {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let mut operations = Vec::new();
    for (path, item) in doc["paths"].as_object().expect("paths") {
        for (key, operation) in item.as_object().expect("path item") {
            let scope = operation
                .pointer("/security/0/session/0")
                .and_then(Value::as_str);
            if matches!(scope, Some("user" | "curator")) {
                operations.push((key.to_uppercase(), path.clone()));
            }
        }
    }
    operations
}

/// Every user or curator operation with a path parameter is a row here
/// or exempt with a reason, and nothing listed here is stale.
#[test]
fn every_path_id_operation_is_in_the_table_or_exempt() {
    let operations = gated_operations();
    let has = |method: &str, path: &str| {
        operations
            .iter()
            .any(|(m, p)| m.as_str() == method && p.as_str() == path)
    };
    let mut wrong = Vec::new();
    for (_, method, path, _, _) in PATH_ID_ROWS {
        if !has(method, path) {
            wrong.push(format!(
                "row {method} {path} names no user or curator operation"
            ));
        }
    }
    for (method, path) in CALLER_KEYED_OPERATIONS.iter().chain(BODY_ID_OPERATIONS) {
        if !has(method, path) {
            wrong.push(format!(
                "row {method} {path} names no user or curator operation"
            ));
        }
    }
    for (method, path, _) in EXACT_EXEMPTIONS {
        if !has(method, path) {
            wrong.push(format!(
                "exemption {method} {path} names no user or curator operation"
            ));
        }
    }
    for (prefix, _) in EXEMPT_READS {
        if !operations.iter().any(|(_, p)| p.starts_with(prefix)) {
            wrong.push(format!(
                "exempt family {prefix} has no user or curator operation"
            ));
        }
    }
    for (method, path) in &operations {
        if !path.contains('{') {
            continue;
        }
        let listed = PATH_ID_ROWS
            .iter()
            .any(|(_, m, p, _, _)| m == method && p == path)
            || CALLER_KEYED_OPERATIONS
                .iter()
                .any(|(m, p)| m == method && p == path)
            || EXACT_EXEMPTIONS
                .iter()
                .any(|(m, p, _)| m == method && p == path);
        let read = method == "GET" || method == "HEAD";
        let exempt_read = read
            && EXEMPT_READS
                .iter()
                .any(|(prefix, _)| path.starts_with(prefix));
        if !listed && !exempt_read {
            wrong.push(format!(
                "{method} {path} takes a path id: add an ownership row or an exemption with its reason"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The ids one seeded resource fills its rows' paths with.
type Ids = HashMap<&'static str, String>;

fn fill(template: &str, ids: &Ids) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest.find('}').expect("balanced braces");
        let name = &rest[start + 1..end];
        out.push_str(
            ids.get(name)
                .unwrap_or_else(|| panic!("{template}: no id for {name}")),
        );
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

const PNG: &str = "iVBORw0KGgo=";

fn track() -> Value {
    json!({"track_name": "One", "artist_name": "Someone", "album_name": "Something"})
}

/// A body the operation decodes, so the answer is about ownership and
/// never about the body.
fn body(method: &str, template: &str, ids: &Ids) -> Option<Value> {
    let id = |name: &str| ids.get(name).cloned().unwrap_or_default();
    match (method, template) {
        ("PUT", "/api/v3/playlists/{playlist_id}") => Some(json!({"name": "Renamed"})),
        ("POST", "/api/v3/playlists/{playlist_id}/cover") => {
            Some(json!({"content_type": "image/png", "image_base64": PNG}))
        }
        ("POST", "/api/v3/playlists/{playlist_id}/tracks") => Some(json!({"tracks": [track()]})),
        ("PATCH", "/api/v3/playlists/{playlist_id}/tracks/reorder") => {
            Some(json!({"track_id": id("track_id"), "new_position": 0}))
        }
        ("PATCH", "/api/v3/playlists/{playlist_id}/tracks/{track_id}") => {
            Some(json!({"source_type": "local"}))
        }
        ("PATCH", "/api/v3/playlists/{playlist_id}/visibility") => {
            Some(json!({"is_public": id("visibility") != "public"}))
        }
        ("POST", "/api/v3/playlists/{playlist_id}/tracks/remove") => {
            Some(json!({"track_ids": [id("track_id")]}))
        }
        ("POST", "/api/v3/held/{entry_id}/match") => {
            Some(json!({"release_group_mbid": "00000000-0000-0000-0000-0000000000aa"}))
        }
        ("POST", "/api/v3/downloads/tasks/{task_id}/next-source") => {
            Some(json!({"expected_candidate_index": 0}))
        }
        ("POST", "/api/v3/downloads/search/{job_id}/pick") => Some(json!({"candidate_index": 0})),
        ("POST", "/api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply") => Some(json!({
            "preview_token": "not-the-token",
            "expected_row_revision": 0,
            "idempotency_key": format!("apply-{}", id("job_id")),
        })),
        ("POST", "/api/v3/library/albums/{album_id}/edition/tags/{job_id}/undo") => Some(json!({
            "expected_row_revision": 0,
            "idempotency_key": format!("undo-{}", id("job_id")),
        })),
        ("POST" | "PUT" | "PATCH", _) => Some(json!({})),
        _ => None,
    }
}

/// Seed one resource of `kind` for `owner`. `n` keeps instances apart.
async fn seed(app: &TestApp, kind: Owned, owner: &Who, n: u32) -> Ids {
    let mut ids = Ids::new();
    match kind {
        Owned::Playlist | Owned::PublicPlaylist => {
            let (status, playlist) = call(
                app,
                owner,
                "POST",
                "/api/v3/playlists",
                Some(json!({"name": format!("Mine {n}")})),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{playlist}");
            let playlist_id = playlist["id"].as_str().expect("playlist id").to_owned();
            let (status, added) = call(
                app,
                owner,
                "POST",
                &format!("/api/v3/playlists/{playlist_id}/tracks"),
                Some(json!({"tracks": [track(), track()]})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{added}");
            let track_id = added["tracks"][0]["id"]
                .as_str()
                .expect("track id")
                .to_owned();
            let (status, cover) = call(
                app,
                owner,
                "POST",
                &format!("/api/v3/playlists/{playlist_id}/cover"),
                Some(json!({"content_type": "image/png", "image_base64": PNG})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{cover}");
            if kind == Owned::PublicPlaylist {
                let (status, shown) = call(
                    app,
                    owner,
                    "PATCH",
                    &format!("/api/v3/playlists/{playlist_id}/visibility"),
                    Some(json!({"is_public": true})),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{shown}");
                ids.insert("visibility", "public".to_owned());
            }
            ids.insert("playlist_id", playlist_id);
            ids.insert("track_id", track_id);
        }
        Owned::Batch => {
            let (status, batch) = call(
                app,
                owner,
                "POST",
                "/api/v3/discover/batches",
                Some(json!({
                    "name": format!("Batch {n}"),
                    "items": [{
                        "musicbrainz_id": format!("00000000-0000-0000-0001-{n:012}"),
                        "title": "Something",
                        "artist_name": "Someone",
                    }],
                })),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{batch}");
            ids.insert(
                "batch_id",
                batch["id"].as_str().expect("batch id").to_owned(),
            );
        }
        Owned::AppPassword => {
            let (status, made) = call(
                app,
                owner,
                "POST",
                "/api/v3/me/app-passwords",
                Some(json!({"name": format!("phone {n}")})),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{made}");
            ids.insert("id", made["id"].as_str().expect("password id").to_owned());
        }
        Owned::Session => {
            let (status, signed_in) = app
                .call(
                    "POST",
                    "/api/v3/auth/login",
                    &[],
                    Some(json!({
                        "username": owner.username,
                        "password": format!("{}-password-1234", owner.username),
                        "transport": "bearer",
                    })),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{signed_in}");
            let fresh = Who {
                id: owner.id.clone(),
                username: owner.username.clone(),
                auth: format!("Bearer {}", signed_in["token"].as_str().expect("token")),
            };
            let (status, sessions) = call(app, &fresh, "GET", "/api/v3/auth/sessions", None).await;
            assert_eq!(status, StatusCode::OK, "{sessions}");
            let current = sessions["sessions"]
                .as_array()
                .expect("sessions")
                .iter()
                .find(|session| session["current"] == json!(true))
                .expect("the new session is listed");
            ids.insert("id", current["id"].as_str().expect("session id").to_owned());
        }
        Owned::Avatar => {
            let (status, me) = call(
                app,
                owner,
                "POST",
                "/api/v3/me/avatar",
                Some(json!({"content_type": "image/png", "image_base64": PNG})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{me}");
            ids.insert("id", owner.id.clone());
        }
        Owned::Request | Owned::LiveRequest => {
            let mbid = format!("00000000-0000-0000-0002-{n:012}");
            let (status, asked) = call(
                app,
                owner,
                "POST",
                "/api/v3/requests/albums",
                Some(json!({"musicbrainz_id": mbid, "artist": "Someone", "album": "Something"})),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{asked}");
            if kind == Owned::LiveRequest {
                ids.insert("musicbrainz_id", mbid);
                return ids;
            }
            // Failed, so a retry reaches the requester check.
            let failed = app
                .state()
                .acquire
                .flows
                .ledger
                .update_status(
                    RequestKind::Album,
                    &mbid,
                    RequestStatus::Failed,
                    Some(1_700_000_000),
                    None,
                )
                .await
                .expect("request fails");
            assert!(failed);
            ids.insert("musicbrainz_id", mbid);
        }
        Owned::TrackRequest => {
            let mbid = format!("00000000-0000-0000-0004-{n:012}");
            let (status, asked) = call(
                app,
                owner,
                "POST",
                "/api/v3/requests/tracks",
                Some(json!({
                    "recording_mbid": mbid,
                    "artist_name": "Someone",
                    "track_title": "Something",
                })),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{asked}");
            let failed = app
                .state()
                .acquire
                .flows
                .ledger
                .update_status(
                    RequestKind::Track,
                    &mbid,
                    RequestStatus::Failed,
                    Some(1_700_000_000),
                    None,
                )
                .await
                .expect("request fails");
            assert!(failed);
            ids.insert("musicbrainz_id", mbid);
        }
        Owned::Wanted => {
            let mbid = format!("00000000-0000-0000-0003-{n:012}");
            let enrolled = app
                .state()
                .acquire
                .flows
                .watches
                .enrol(WantedWatch {
                    key: mbid.clone(),
                    user_id: owner.id.clone(),
                    user_name: None,
                    artist_name: "Someone".to_owned(),
                    album_title: "Something".to_owned(),
                    artist_mbid: None,
                    year: None,
                    cover_url: None,
                    kind: "missing".to_owned(),
                    state: WatchState::Stopped,
                    created_at: 1_700_000_000,
                    first_release_date: None,
                    check_count: 0,
                    quiet_streak: 0,
                    next_check_at: u64::MAX / 2,
                    new_candidate_count: 1,
                })
                .await
                .expect("watch enrols");
            assert!(enrolled);
            ids.insert("musicbrainz_id", mbid);
        }
        Owned::Task | Owned::Held | Owned::HeldMatch => {
            let task_id = format!("{:032x}", 0xa0000 + u64::from(n));
            let task = NewTask {
                id: task_id.clone(),
                user_id: owner.id.clone(),
                artist_name: "Someone".to_owned(),
                album_title: "Something".to_owned(),
                release_group_mbid: format!("00000000-0000-0000-0004-{n:012}"),
                origin: "user".to_owned(),
                retry_count: 9,
            };
            let user_id = owner.id.clone();
            let held = kind != Owned::Task;
            let held_dir = app.config.cache_dir.join("held");
            std::fs::create_dir_all(&held_dir).expect("held folder");
            let held_name = format!("{n} One.flac");
            std::fs::write(held_dir.join(&held_name), b"not really audio").expect("held file");
            app.state()
                .acquire
                .journal
                .run("test.seed_task", move |store| {
                    store.insert_task(&task, 1_700_000_000.0)?;
                    store.transition_task(
                        &task.id,
                        TaskStatus::Failed,
                        1_700_000_000.0,
                        Some(TaskOutcome::new(Reason::DownloadFailed, "seeded")),
                    )
                })
                .await
                .expect("task seeds");
            if held {
                let (group, task) = (format!("00000000-0000-0000-0004-{n:012}"), task_id.clone());
                let (entry_id, files) = app
                    .state()
                    .acquire
                    .db
                    .write("test.seed_held", move |tx| {
                        Ok(HeldRows::new(tx).hold(
                            &NewEntry {
                                user_id,
                                origin: EntryOrigin::Download,
                                key: GroupKey::Album(group.clone()),
                                release_group_mbid: Some(group),
                                recording_mbid: None,
                                drop_item_id: None,
                                artist_name: Some("Someone".to_owned()),
                                album_title: Some("Something".to_owned()),
                            },
                            &[NewFile {
                                source_task_id: Some(task),
                                ..NewFile::new(
                                    HeldRoot::Held,
                                    &held_name,
                                    Reason::FingerprintMismatch,
                                    "soulseek",
                                )
                            }],
                            1_700_000_000_000,
                        )?)
                    })
                    .await
                    .expect("held seeds");
                ids.insert("entry_id", entry_id.to_string());
                ids.insert("file_id", files[0].expect("a new held file").to_string());
                ids.insert("source_task_id", task_id);
            } else {
                ids.insert("task_id", task_id);
            }
        }
        Owned::SearchJob => {
            let job_id = format!("search-job-{n}");
            let jobs = JobStore::new(app.state().acquire.db.clone());
            jobs.insert(NewJob {
                id: job_id.clone(),
                user_id: owner.id.clone(),
                artist_name: "Someone".to_owned(),
                album_title: "Something".to_owned(),
                year: None,
                release_group_mbid: None,
                release_mbid: None,
            })
            .await
            .expect("job seeds");
            assert!(
                jobs.finish(&job_id, COMPLETED, JobPayload::default(), None)
                    .await
                    .expect("job finishes")
            );
            ids.insert("job_id", job_id);
        }
        Owned::SpotifyJob => {
            let spotify_id = format!("spotify-list-{n}");
            let imports = &app.state().acquire.imports;
            let playlist_id = imports
                .spotify_playlists
                .create(&owner.id, "Imported", &format!("spotify:{spotify_id}"))
                .await
                .expect("playlist seeds");
            imports
                .spotify_imports
                .seed_done(&owner.id, &spotify_id, &playlist_id)
                .await
                .expect("import seeds");
            ids.insert("id", spotify_id);
        }
        Owned::TagWrite => {
            let album = format!("tag-write-album-{n}");
            let job = format!("tag-write-job-{n}");
            seed_tag_write(app, owner, &album, &job);
            ids.insert("album_id", album);
            ids.insert("job_id", job);
        }
        Owned::Drop => {
            let job = drop_upload(app, owner).await;
            ids.insert("job_id", job["id"].as_str().expect("job id").to_owned());
            ids.insert(
                "item_id",
                job["items"][0]["id"]
                    .to_string()
                    .trim_matches('"')
                    .to_owned(),
            );
        }
    }
    ids
}

/// A planned tag write of `album`'s edition that `owner` asked for, waiting
/// to be applied, so the management worker leaves it as it is.
fn seed_tag_write(app: &TestApp, owner: &Who, album: &str, job: &str) {
    use droppedneedle_library::management::models::{
        EditionPin, JobIntent, Origin, Revisions, Summary,
    };
    use droppedneedle_library::management::store::{NewJob, insert_job};
    let mut db = app.db();
    let tx = db.transaction().expect("transaction opens");
    insert_job(
        &tx,
        &NewJob {
            id: job,
            requested_by: Some(&owner.id),
            idempotency_key: None,
            mode: "preview",
            selection_json: r#"{"kind":"tracks","ids":[]}"#,
            profile_snapshot_json: "{}",
            revisions: &Revisions {
                profile: String::new(),
                settings: String::new(),
                naming: String::new(),
                policy: String::new(),
                overrides: String::new(),
            },
            catalog_revision: 0,
            token_hash: "",
            now: 1.0,
            expires_at: 1e12,
            target_root_id: None,
            linked_job_id: None,
            summary: &Summary::default(),
            origin: Origin::Manual,
            intent: &JobIntent {
                edition: Some(EditionPin {
                    local_album_id: album.to_owned(),
                    release_mbid: "00000000-0000-0000-0000-0000000000bb".to_owned(),
                    identity_revision: 1,
                }),
                ..JobIntent::default()
            },
            proposed_settings_revision: None,
        },
    )
    .expect("tag write seeds");
    tx.execute_batch(&format!(
        "UPDATE library_operation_jobs SET state = 'ready' WHERE id = '{job}'; \
         UPDATE library_management_job_snapshots SET phase = 'ready' WHERE job_id = '{job}';"
    ))
    .expect("tag write is planned");
    tx.commit().expect("tag write lands");
}

/// Upload one audio file to the drop import as `owner` and wait for the
/// job to settle with its item.
async fn drop_upload(app: &TestApp, owner: &Who) -> Value {
    let audio = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/library/flac_full_01.flac"
    ))
    .expect("fixture reads");
    let boundary = "ownership-boundary";
    let mut form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"01 One.flac\"\r\n\
         Content-Type: audio/flac\r\n\r\n"
    )
    .into_bytes();
    form.extend_from_slice(&audio);
    form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    app.state().auth.limits.reset();
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, bytes) = app
        .raw_bytes(
            "POST",
            "/api/v3/import/uploads",
            &[
                ("authorization", owner.auth.as_str()),
                ("content-type", content_type.as_str()),
            ],
            form,
        )
        .await;
    let job: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(status.is_success(), "upload: {status} {job}");
    let job_id = job["id"].as_str().expect("job id").to_owned();
    app.until("the drop job settles with its item", || async {
        let (_, job) = call(
            app,
            owner,
            "GET",
            &format!("/api/v3/import/jobs/{job_id}"),
            None,
        )
        .await;
        (job["status"] != json!("processing") && job["items"][0]["id"] != Value::Null)
            .then_some(job)
    })
    .await
}

/// What `owner` sees of the resource: the fields another user's call
/// could change.
async fn look(app: &TestApp, kind: Owned, owner: &Who, ids: &Ids) -> Value {
    let read = |uri: String| async move {
        let (status, body) = call(app, owner, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        body
    };
    match kind {
        Owned::Playlist | Owned::PublicPlaylist => {
            let playlist = read(fill("/api/v3/playlists/{playlist_id}", ids)).await;
            json!([
                playlist["name"],
                playlist["is_public"],
                playlist["custom_cover_url"],
                playlist["tracks"]
            ])
        }
        Owned::Batch => read(fill("/api/v3/discover/batches/{batch_id}", ids)).await,
        Owned::AppPassword => read("/api/v3/me/app-passwords".to_owned()).await,
        Owned::Session => {
            let sessions = read("/api/v3/auth/sessions".to_owned()).await;
            json!(
                sessions["sessions"]
                    .as_array()
                    .expect("sessions")
                    .iter()
                    .map(|session| session["id"].clone())
                    .collect::<Vec<_>>()
            )
        }
        Owned::Avatar => read("/api/v3/me".to_owned()).await["avatar_url"].clone(),
        Owned::Request | Owned::LiveRequest | Owned::TrackRequest => json!([
            read("/api/v3/requests/active".to_owned()).await,
            read("/api/v3/requests/history".to_owned()).await,
        ]),
        Owned::Wanted => read("/api/v3/requests/wanted".to_owned()).await,
        Owned::Task => read(fill("/api/v3/downloads/tasks/{task_id}", ids)).await,
        Owned::Held | Owned::HeldMatch => json!([
            read("/api/v3/held".to_owned()).await,
            read(format!("/api/v3/downloads/tasks/{}", ids["source_task_id"])).await,
        ]),
        Owned::SearchJob => read(fill("/api/v3/downloads/search/{job_id}", ids)).await,
        Owned::SpotifyJob => read(fill("/api/v3/acquire/spotify/jobs/{id}", ids)).await,
        Owned::Drop => read(fill("/api/v3/import/jobs/{job_id}", ids)).await,
        Owned::TagWrite => {
            read(fill(
                "/api/v3/library/albums/{album_id}/edition/tags/{job_id}",
                ids,
            ))
            .await
        }
    }
}

fn answered(status: StatusCode, expected: u16) -> bool {
    if expected == ADMITTED {
        !matches!(status.as_u16(), 401 | 403 | 404) && !status.is_server_error()
    } else {
        status.as_u16() == expected
    }
}

/// Every path-id row, for the other user, the admin and the owner.
#[tokio::test]
async fn another_users_resources_stay_out_of_reach() {
    let app = TestApp::start("auth-ownership").await;
    let admin = Who {
        id: String::new(),
        username: "admin".to_owned(),
        auth: app.login_admin().await,
    };
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;
    let tia = trusted(&app, &admin, "tia").await;
    let tom = trusted(&app, &admin, "tom").await;
    // The drop import needs a library folder to import into.
    let music = app.config.root_app_dir.join("music");
    std::fs::create_dir_all(&music).expect("music folder");
    app.add_library_folder("music", &music).await;
    let families = [
        Owned::Playlist,
        Owned::PublicPlaylist,
        Owned::Batch,
        Owned::AppPassword,
        Owned::Session,
        Owned::Avatar,
        Owned::Request,
        Owned::LiveRequest,
        Owned::TrackRequest,
        Owned::Wanted,
        Owned::Task,
        Owned::Held,
        Owned::HeldMatch,
        Owned::SearchJob,
        Owned::SpotifyJob,
        Owned::Drop,
        Owned::TagWrite,
    ];
    let mut wrong = Vec::new();
    for (n, kind) in (0u32..).step_by(2).zip(families) {
        let (owner, other) = if matches!(kind, Owned::Drop | Owned::TagWrite | Owned::HeldMatch) {
            (&tia, &tom)
        } else {
            (&ada, &bob)
        };
        let rows: Vec<_> = PATH_ID_ROWS.iter().filter(|row| row.0 == kind).collect();

        // The other user: refused, and the owner's resource unchanged.
        let ids = seed(&app, kind, owner, n).await;
        let before = look(&app, kind, owner, &ids).await;
        for (_, method, template, refused, _) in &rows {
            let uri = fill(template, &ids);
            let (status, body) =
                call(&app, other, method, &uri, body(method, template, &ids)).await;
            if !answered(status, *refused) {
                wrong.push(format!(
                    "other user {method} {template}: {status}, pinned {refused}: {body}"
                ));
            }
        }
        let after = look(&app, kind, owner, &ids).await;
        if before != after {
            wrong.push(format!(
                "{kind:?}: the other user's calls changed it: {before} -> {after}"
            ));
        }

        // The owner, in row order, destructive rows last.
        for (_, method, template, _, _) in &rows {
            let uri = fill(template, &ids);
            let (status, body) =
                call(&app, owner, method, &uri, body(method, template, &ids)).await;
            if !answered(status, ADMITTED) {
                wrong.push(format!("owner {method} {template}: {status}: {body}"));
            }
        }

        // The admin, on a resource of its own seeding.
        let ids = seed(&app, kind, owner, n + 1).await;
        for (_, method, template, _, pinned) in &rows {
            let uri = fill(template, &ids);
            let (status, body) =
                call(&app, &admin, method, &uri, body(method, template, &ids)).await;
            if !answered(status, *pinned) {
                wrong.push(format!(
                    "admin {method} {template}: {status}, pinned {pinned}: {body}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    app.stop().await;
}

/// Favourites, ratings, follows, play marks, remote-server links and
/// Navidrome folder choices belong to their caller: A's never show in B's
/// reads, and B's writes never touch A's.
#[tokio::test]
async fn caller_keyed_state_stays_with_its_caller() {
    let app = TestApp::start("auth-caller-keyed").await;
    let admin = Who {
        id: String::new(),
        username: "admin".to_owned(),
        auth: app.login_admin().await,
    };
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;
    let ok = |status: StatusCode, body: &Value, what: &str| {
        assert!(status.is_success(), "{what}: {status} {body}");
    };

    // Favourites.
    let album = "00000000-0000-0000-0005-000000000001";
    let favorite = format!("/api/v3/favorites/album/{album}");
    let (status, body) = call(
        &app,
        &ada,
        "PUT",
        &favorite,
        Some(json!({"favorited": true})),
    )
    .await;
    ok(status, &body, "A favourites");
    let (status, body) = call(&app, &bob, "GET", "/api/v3/favorites", None).await;
    ok(status, &body, "B lists favourites");
    assert!(
        !body.to_string().contains(album),
        "B sees A's favourite: {body}"
    );
    let (status, body) = call(
        &app,
        &bob,
        "PUT",
        &favorite,
        Some(json!({"favorited": false})),
    )
    .await;
    ok(status, &body, "B unfavourites");
    let (_, body) = call(&app, &ada, "GET", "/api/v3/favorites", None).await;
    assert!(
        body.to_string().contains(album),
        "B's write took A's favourite: {body}"
    );

    // Ratings, on a library album.
    app.db()
        .execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
             VALUES ('own-artist', 'Someone', 'someone', 'person', 0, 0); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) VALUES \
             ('own-album', 'root-1', 'g-own', 'Something', 'something', 'own-artist', 'manual', 0, 0);",
        )
        .expect("library album seeds");
    let rating = "/api/v3/ratings/album/own-album";
    let lookup = Some(json!({"album_ids": ["own-album"]}));
    let (status, body) = call(&app, &ada, "PUT", rating, Some(json!({"rating": 4}))).await;
    ok(status, &body, "A rates");
    let (status, body) = call(&app, &bob, "POST", "/api/v3/ratings/lookup", lookup.clone()).await;
    ok(status, &body, "B looks up ratings");
    assert_eq!(body["albums"], json!({}), "B sees A's rating");
    let (status, body) = call(&app, &bob, "PUT", rating, Some(json!({"rating": 0}))).await;
    ok(status, &body, "B clears a rating");
    let (_, body) = call(&app, &ada, "POST", "/api/v3/ratings/lookup", lookup).await;
    assert_eq!(
        body["albums"]["own-album"],
        json!(4),
        "B's write took A's rating: {body}"
    );

    // Follows and auto-download.
    let artist = "00000000-0000-0000-0006-000000000001";
    let follow = format!("/api/v3/artists/{artist}/follow");
    let auto = format!("/api/v3/artists/{artist}/auto-download");
    let status_uri = format!("/api/v3/artists/{artist}/follow-status");
    let (status, body) = call(
        &app,
        &ada,
        "PUT",
        &follow,
        Some(json!({"followed": true, "artist_name": "Someone"})),
    )
    .await;
    ok(status, &body, "A follows");
    let (status, body) = call(&app, &ada, "PUT", &auto, Some(json!({"enabled": true}))).await;
    ok(status, &body, "A asks for auto-download");
    let (_, adas) = call(&app, &ada, "GET", &status_uri, None).await;
    let (status, body) = call(&app, &bob, "GET", &status_uri, None).await;
    ok(status, &body, "B reads follow status");
    assert_eq!(
        (
            body["followed"].clone(),
            body["auto_download_state"].clone()
        ),
        (json!(false), json!("none")),
        "B sees A's follow"
    );
    let (_, body) = call(&app, &bob, "GET", "/api/v3/following/artists", None).await;
    assert!(
        !body.to_string().contains(artist),
        "B lists A's follow: {body}"
    );
    call(&app, &bob, "PUT", &auto, Some(json!({"enabled": false}))).await;
    call(&app, &bob, "PUT", &follow, Some(json!({"followed": false}))).await;
    let (_, body) = call(&app, &ada, "GET", &status_uri, None).await;
    assert_eq!(body, adas, "B's writes changed A's follow");

    // Play marks: the snapshot shows everyone by design, and B's stop
    // never ends A's.
    let (status, body) = call(
        &app,
        &ada,
        "POST",
        "/api/v3/now-playing",
        Some(json!({"track_name": "One", "artist_name": "Someone", "device": "web"})),
    )
    .await;
    ok(status, &body, "A plays");
    let (status, body) = call(&app, &bob, "DELETE", "/api/v3/now-playing?device=web", None).await;
    ok(status, &body, "B stops");
    let (_, body) = call(&app, &ada, "GET", "/api/v3/now-playing", None).await;
    assert!(
        body.to_string().contains("ada"),
        "B's stop ended A's play: {body}"
    );

    // Remote-server links and folder choices, against the mock Navidrome.
    let navidrome = droppedneedle_testkit::remotes::serve_navidrome()
        .await
        .expect("mock Navidrome serves");
    let (status, mut settings) =
        call(&app, &admin, "GET", "/api/v3/settings/navidrome", None).await;
    ok(status, &settings, "admin reads Navidrome settings");
    settings["navidrome_url"] = json!(navidrome.base_url);
    settings["enabled"] = json!(true);
    let (status, body) = call(
        &app,
        &admin,
        "PUT",
        "/api/v3/settings/navidrome",
        Some(settings),
    )
    .await;
    ok(status, &body, "admin saves Navidrome settings");
    let link = Some(json!({
        "username": droppedneedle_testkit::remotes::NAVIDROME_USER,
        "password": "pw",
    }));
    let connection = "/api/v3/remotes/navidrome/connection";
    let (status, body) = call(&app, &ada, "PUT", connection, link.clone()).await;
    ok(status, &body, "A links Navidrome");
    let (status, body) = call(&app, &bob, "GET", connection, None).await;
    ok(status, &body, "B reads the link");
    assert_eq!(body["connected"], json!(false), "B sees A's link: {body}");
    let (status, body) = call(&app, &bob, "DELETE", connection, None).await;
    ok(status, &body, "B unlinks");
    let (_, body) = call(&app, &ada, "GET", connection, None).await;
    assert_eq!(
        body["connected"],
        json!(true),
        "B's unlink ended A's: {body}"
    );

    let folders = "/api/v3/remotes/navidrome/folders";
    let (status, body) = call(
        &app,
        &ada,
        "PUT",
        folders,
        Some(json!({"mode": "selected", "selected_folder_ids": ["folder-1"]})),
    )
    .await;
    ok(status, &body, "A picks folders");
    let (status, body) = call(&app, &bob, "PUT", connection, link).await;
    ok(status, &body, "B links Navidrome");
    let (status, body) = call(&app, &bob, "GET", folders, None).await;
    ok(status, &body, "B reads folders");
    assert_eq!(
        body["mode"],
        json!("all"),
        "B sees A's folder choice: {body}"
    );
    let (status, body) = call(
        &app,
        &bob,
        "PUT",
        folders,
        Some(json!({"mode": "all", "selected_folder_ids": []})),
    )
    .await;
    ok(status, &body, "B picks all folders");
    let (_, body) = call(&app, &ada, "GET", folders, None).await;
    assert_eq!(
        body["mode"],
        json!("selected"),
        "B's choice changed A's: {body}"
    );
    app.stop().await;
}

/// Operations that name a user-owned resource in the body reach only the
/// caller's own.
#[tokio::test]
async fn body_ids_never_reach_another_users_resources() {
    let app = TestApp::start("auth-body-ids").await;
    let admin = Who {
        id: String::new(),
        username: "admin".to_owned(),
        auth: app.login_admin().await,
    };
    let ada = user(&app, "ada").await;
    let bob = user(&app, "bob").await;

    // Cancelling requests in bulk.
    let ids = seed(&app, Owned::LiveRequest, &ada, 0).await;
    let before = look(&app, Owned::LiveRequest, &ada, &ids).await;
    let (status, body) = call(
        &app,
        &bob,
        "POST",
        "/api/v3/requests/batches/cancel",
        Some(json!({"musicbrainz_ids": [ids["musicbrainz_id"]]})),
    )
    .await;
    assert_eq!(
        (status, body["cancelled"].clone()),
        (StatusCode::OK, json!(0)),
        "{body}"
    );
    assert_eq!(
        look(&app, Owned::LiveRequest, &ada, &ids).await,
        before,
        "B cancelled A's request"
    );

    // Re-checking held files in bulk.
    let ids = seed(&app, Owned::Held, &ada, 1).await;
    let before = look(&app, Owned::Held, &ada, &ids).await;
    let entry_id: i64 = ids["entry_id"].parse().expect("numeric entry id");
    let (status, body) = call(
        &app,
        &bob,
        "POST",
        "/api/v3/held/recheck",
        Some(json!({"entry_ids": [entry_id]})),
    )
    .await;
    assert_eq!(
        (status, body["results"].clone()),
        (StatusCode::OK, json!([])),
        "B reached A's held file: {body}"
    );
    assert_eq!(
        look(&app, Owned::Held, &ada, &ids).await,
        before,
        "B re-checked A's held file"
    );

    // Suggestions for a private playlist: only its owner, never an admin,
    // since admins never see another user's private playlist.
    let ids = seed(&app, Owned::Playlist, &ada, 2).await;
    let suggest = Some(json!({"playlist_id": ids["playlist_id"]}));
    let uri = "/api/v3/discover/playlist-suggestions";
    let (status, body) = call(&app, &bob, "POST", uri, suggest.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "other user: {body}");
    let (status, body) = call(&app, &admin, "POST", uri, suggest.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "admin: {body}");
    let (status, body) = call(&app, &ada, "POST", uri, suggest).await;
    assert!(answered(status, ADMITTED), "owner: {status} {body}");
    app.stop().await;
}
