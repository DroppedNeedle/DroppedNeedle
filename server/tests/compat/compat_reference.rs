//! The compat APIs against what a real Navidrome and a real Jellyfin
//! answered for the same requests over the same six tracks.
//!
//! Each row of a recording's `index.json` is sent to the app and both
//! answers are reduced to a shape: every key path with its type, array
//! positions collapsed. A key the real server returns must be in the app's
//! answer with the same type. Values are not compared, except the three a
//! client branches on: the HTTP status, Subsonic's `status` and its
//! `error.code`. Audio and image answers compare the status, the content
//! type and which framing headers are present.
//!
//! A difference passes only while it is listed, with a reason, in one of the
//! files of `tests/fixtures/compat/known_differences/`, read in name order.
//! Listed means known, not approved. `accepted.json` holds the differences
//! the owner accepted as they are: streaming needs a token, and the
//! transcoder is off in these tests. An entry that matches no difference
//! fails too, as does an entry that names the wrong requests.
//!
//! The recordings are read-only: one test recomputes their checksums and
//! repeats the recorder's sweep for anything private.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::common::compat::{CompatApp, PLAYER};

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/reference/compat")
}

fn known_differences_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat/known_differences")
}

/// Every `*.json` file in the known differences folder, in name order, with
/// the entries each one lists. A folder with no such file is an error, so a
/// wrong path cannot pass as "nothing is known".
fn known_differences() -> Vec<(String, Vec<Known>)> {
    let dir = known_differences_dir();
    let listing = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let mut names: Vec<String> = listing
        .map(|entry| {
            let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
            entry.file_name().to_string_lossy().into_owned()
        })
        .filter(|name| name.ends_with(".json"))
        .collect();
    assert!(!names.is_empty(), "{} has no .json file", dir.display());
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let known: KnownDifferences = serde_json::from_value(read_json(&dir.join(&name)))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, known.differences)
        })
        .collect()
}

fn read_json(path: &Path) -> Value {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[derive(Deserialize)]
struct Index {
    requests: Vec<Row>,
}

/// One recorded request, in the form the recorder replays.
#[derive(Deserialize)]
struct Row {
    name: String,
    file: String,
    method: String,
    path: String,
    #[serde(default)]
    query: Vec<(String, String)>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: Option<Value>,
    auth: String,
    answer: String,
    sets: Option<Sets>,
}

/// Where a write's answer carries the id later requests name.
#[derive(Deserialize)]
struct Sets {
    #[serde(rename = "ref")]
    name: String,
    at: Vec<String>,
}

#[derive(Deserialize)]
struct KnownDifferences {
    differences: Vec<Known>,
}

#[derive(Deserialize)]
struct Known {
    server: String,
    object: String,
    key: String,
    kind: String,
    rows: BTreeSet<String>,
    reason: String,
}

/// Object, key and kind: one difference however many requests show it.
type Place = (String, String, String);

/// What was found at one place: per request, the two sides in words.
type Found = BTreeMap<Place, BTreeMap<String, (String, String)>>;

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

type Shape = BTreeMap<String, BTreeSet<&'static str>>;

/// Every key path of `value` with the types seen there. The elements of an
/// array share one path, `<array>[]`.
fn shape(value: &Value, path: &str, out: &mut Shape) {
    out.entry(path.to_owned())
        .or_default()
        .insert(kind_of(value));
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                shape(inner, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            for inner in items {
                shape(inner, &format!("{path}[]"), out);
            }
        }
        _ => {}
    }
}

fn words(kinds: &BTreeSet<&'static str>) -> String {
    kinds.iter().copied().collect::<Vec<_>>().join(" or ")
}

/// `(path, kind, real, ours)` for every key the real answer has that ours
/// lacks or types differently. `null` on the real side is an optional key.
/// An integer where the real server has a float passes.
fn shape_differences(real: &Value, ours: &Value) -> Vec<(String, &'static str, String, String)> {
    let (mut real_shape, mut our_shape) = (Shape::new(), Shape::new());
    shape(real, "body", &mut real_shape);
    shape(ours, "body", &mut our_shape);
    let mut found = Vec::new();
    let mut settled: Vec<String> = Vec::new();
    for (path, real_kinds) in &real_shape {
        let below = |parent: &String| {
            path.strip_prefix(parent.as_str())
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
        };
        if settled.iter().any(below) {
            continue;
        }
        let mut wanted = real_kinds.clone();
        wanted.remove("null");
        if wanted.is_empty() {
            continue;
        }
        let Some(got) = our_shape.get(path) else {
            let list = path.strip_suffix("[]").filter(|list| {
                our_shape
                    .get(*list)
                    .is_some_and(|kinds| kinds.contains("array"))
            });
            match list {
                Some(list) => found.push((
                    list.to_owned(),
                    "empty",
                    format!("a list of {}", words(&wanted)),
                    "an empty list: the seed is short, so nothing in it was compared".to_owned(),
                )),
                None => found.push((
                    path.clone(),
                    "missing",
                    words(&wanted),
                    "nothing".to_owned(),
                )),
            }
            settled.push(path.clone());
            continue;
        };
        let same = wanted
            .iter()
            .all(|kind| got.contains(kind) || (*kind == "float" && got.contains("integer")));
        if !same {
            found.push((path.clone(), "type", words(&wanted), words(got)));
            settled.push(path.clone());
        }
    }
    found
}

/// `body.a.b[].c` is key `c` of object `b`.
fn place(path: &str, kind: &str) -> Place {
    let (parent, key) = path.rsplit_once('.').unwrap_or(("body", path));
    let object = parent.rsplit('.').next().unwrap_or(parent);
    (
        object.trim_end_matches("[]").to_owned(),
        key.to_owned(),
        kind.to_owned(),
    )
}

/// Percent-encoding as the recorder's `encodeURIComponent` did it, so a
/// query reaches the app as it reached the real server.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// One recorded server and the app standing in for it.
struct Comparison<'a> {
    rig: &'a CompatApp,
    server: &'static str,
    /// What `{name}` in a request stands for on the app.
    refs: BTreeMap<String, String>,
    found: Found,
}

impl<'a> Comparison<'a> {
    fn fill(&self, template: &str) -> String {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                break;
            };
            let name = &rest[start + 1..start + len];
            let Some(value) = self.refs.get(name) else {
                // Not a placeholder: `{"a":1}` in a query value, say.
                out.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
                continue;
            };
            out.push_str(&rest[..start]);
            out.push_str(value);
            rest = &rest[start + len + 1..];
        }
        out.push_str(rest);
        out
    }

    fn fill_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.fill(text)),
            Value::Array(items) => items.iter().map(|item| self.fill_value(item)).collect(),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, inner)| (key.clone(), self.fill_value(inner)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// The row as the app is asked it: the same path, query, headers and
    /// body under the app's mount point, with the player's credentials
    /// where the recorder put its own.
    async fn send(&self, row: &Row) -> (u16, HeaderMap, Vec<u8>) {
        let mut query: Vec<(String, String)> = Vec::new();
        let mut headers: Vec<(String, String)> = row
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), self.fill(value)))
            .collect();
        let mount = if self.server == "navidrome" {
            if row.auth != "none" {
                let password = if row.auth == "wrong_password" {
                    "wrong-password"
                } else {
                    self.rig.secret.as_str()
                };
                query.push(("u".to_owned(), PLAYER.to_owned()));
                query.push(("p".to_owned(), password.to_owned()));
            }
            for (key, value) in [("v", "1.16.1"), ("c", "dn-ref"), ("f", "json")] {
                query.push((key.to_owned(), value.to_owned()));
            }
            "/subsonic"
        } else {
            let identity = "MediaBrowser Client=\"dn-ref\", Device=\"dn-ref\", DeviceId=\"dn-ref-device\", Version=\"1.0\"";
            match row.auth.as_str() {
                "none" => {}
                "api_key" => query.push(("api_key".to_owned(), self.rig.secret.clone())),
                "client" => headers.push(("authorization".to_owned(), identity.to_owned())),
                _ => headers.push((
                    "authorization".to_owned(),
                    format!("{identity}, Token=\"{}\"", self.rig.secret),
                )),
            }
            "/jellyfin"
        };
        for (key, value) in &row.query {
            // The recorder asked for its own account by name.
            let value = if key == "username" {
                PLAYER.to_owned()
            } else {
                self.fill(value)
            };
            query.push((key.clone(), value));
        }
        let query: Vec<String> = query
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect();
        let mut uri = format!("{mount}{}", self.fill(&row.path));
        if !query.is_empty() {
            uri = format!("{uri}?{}", query.join("&"));
        }
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let body = row.body.as_ref().map(|body| self.fill_value(body));
        let (status, headers, bytes) = self.rig.app.raw(&row.method, &uri, &headers, body).await;
        (status.as_u16(), headers, bytes)
    }

    /// A difference inside a body belongs to the object holding the key,
    /// so it is one entry on every request. The status, the headers and a
    /// body of the wrong kind belong to the request.
    fn note(&mut self, row: &str, path: &str, kind: &str, real: String, ours: String) {
        let place = if path.starts_with("body.") || path.starts_with("body[") {
            place(path, kind)
        } else {
            (row.to_owned(), path.to_owned(), kind.to_owned())
        };
        self.found
            .entry(place)
            .or_default()
            .insert(row.to_owned(), (real, ours));
    }

    /// Replay one recording folder. `prefix` tells its rows from those of
    /// the folder it continues.
    async fn replay(&mut self, folder: &str, prefix: &str) {
        let dir = reference_dir().join(folder);
        let index: Index =
            serde_json::from_value(read_json(&dir.join("index.json"))).expect("index parses");
        for row in &index.requests {
            let recorded = read_json(&dir.join(&row.file));
            let name = format!("{prefix}{}", row.name);
            let (status, headers, bytes) = self.send(row).await;
            let ours: Option<Value> = serde_json::from_slice(&bytes).ok();
            if let (Some(sets), Some(ours)) = (&row.sets, &ours) {
                let id = sets.at.iter().fold(ours, |node, key| &node[key.as_str()]);
                if let Some(id) = id.as_str() {
                    self.refs.insert(sets.name.clone(), id.to_owned());
                }
            }

            let real_status = recorded["status"].as_u64().expect("recorded status");
            if real_status != u64::from(status) {
                self.note(
                    &name,
                    "status",
                    "value",
                    real_status.to_string(),
                    status.to_string(),
                );
                continue;
            }
            match row.answer.as_str() {
                "json" => {
                    let real = &recorded["body"];
                    let Some(ours) = ours else {
                        self.note(
                            &name,
                            "body",
                            "type",
                            kind_of(real).to_owned(),
                            "not JSON".to_owned(),
                        );
                        continue;
                    };
                    let subsonic = |body: &Value, key: &str| {
                        let answer = &body["subsonic-response"];
                        match key {
                            "status" => answer["status"].clone(),
                            _ => answer["error"]["code"].clone(),
                        }
                    };
                    let mut branched = false;
                    for key in ["status", "code"] {
                        let (real, ours) = (subsonic(real, key), subsonic(&ours, key));
                        if !real.is_null() && real != ours {
                            let path = if key == "status" {
                                "body.subsonic-response.status"
                            } else {
                                "body.subsonic-response.error.code"
                            };
                            self.note(&name, path, "value", real.to_string(), ours.to_string());
                            branched = true;
                        }
                    }
                    if branched {
                        continue;
                    }
                    for (path, kind, real, ours) in shape_differences(real, &ours) {
                        self.note(&name, &path, kind, real, ours);
                    }
                }
                "bytes" | "unread" => {
                    let real_headers = recorded["headers"].as_object().expect("recorded headers");
                    let ours = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
                    let essence = |kind: &str| {
                        kind.split(';')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_ascii_lowercase()
                    };
                    let real_type = real_headers
                        .get("content-type")
                        .and_then(Value::as_str)
                        .map(essence);
                    let our_type = ours("content-type").map(essence);
                    if real_type.is_some() && real_type != our_type {
                        self.note(
                            &name,
                            "headers.content-type",
                            "value",
                            real_type.unwrap_or_default(),
                            our_type.unwrap_or_else(|| "nothing".to_owned()),
                        );
                    }
                    for framing in ["content-length", "content-range", "accept-ranges"] {
                        if real_headers.contains_key(framing) && ours(framing).is_none() {
                            self.note(
                                &name,
                                &format!("headers.{framing}"),
                                "missing",
                                "present".to_owned(),
                                "nothing".to_owned(),
                            );
                        }
                    }
                }
                // A text or empty answer is an error page: its status is
                // what a client reads.
                _ => {}
            }
        }
    }

    /// Fail on every difference that is not listed and every listed one
    /// that was not found.
    fn settle(self) {
        let mut listed: BTreeMap<Place, (String, BTreeSet<String>)> = BTreeMap::new();
        for (file, differences) in known_differences() {
            for entry in differences {
                assert!(
                    ["navidrome", "jellyfin"].contains(&entry.server.as_str()),
                    "known_differences/{file} names an unknown server: {}",
                    entry.server
                );
                assert!(
                    entry.reason.trim().len() > 20,
                    "known_differences/{file}: {}.{} has no reason",
                    entry.object,
                    entry.key
                );
                if entry.server != self.server {
                    continue;
                }
                let place = (entry.object, entry.key, entry.kind);
                if let Some((first, _)) = listed.get(&place) {
                    panic!("known_differences/ lists {place:?} twice, in {first} and {file}");
                }
                listed.insert(place, (file.clone(), entry.rows));
            }
        }

        let mut unlisted = Vec::new();
        for (place, rows) in &self.found {
            let known = listed.get(place).map(|(_, rows)| rows);
            // One line per pair of answers, so a status of 404 on one
            // request is not hidden behind a 500 on another.
            let mut new: BTreeMap<&(String, String), Vec<&String>> = BTreeMap::new();
            for (row, sides) in rows {
                if known.is_none_or(|known| !known.contains(row)) {
                    new.entry(sides).or_default().push(row);
                }
            }
            for ((real, ours), rows) in new {
                unlisted.push(
                    json!({
                        "server": self.server,
                        "object": place.0,
                        "key": place.1,
                        "kind": place.2,
                        "rows": rows,
                        "real": real,
                        "v3": ours,
                    })
                    .to_string(),
                );
            }
        }
        let mut stale = Vec::new();
        for (place, (file, rows)) in &listed {
            let found = self.found.get(place);
            let gone: Vec<&String> = rows
                .iter()
                .filter(|row| found.is_none_or(|found| !found.contains_key(*row)))
                .collect();
            if !gone.is_empty() {
                stale.push(
                    json!({
                        "file": file,
                        "server": self.server,
                        "object": place.0,
                        "key": place.1,
                        "kind": place.2,
                        "rows": gone,
                    })
                    .to_string(),
                );
            }
        }
        assert!(
            unlisted.is_empty() && stale.is_empty(),
            "{} differences from the real {server} are not in known_differences/:\n{}\n\n{} listed differences were not found (fix the entry or remove it):\n{}\n",
            unlisted.len(),
            unlisted.join("\n"),
            stale.len(),
            stale.join("\n"),
            server = self.server,
        );
    }
}

/// The first object in `items` whose `key` is `name`, read at `id`.
fn id_of(items: &Value, key: &str, name: &Value, id: &str) -> Option<String> {
    items
        .as_array()?
        .iter()
        .find(|item| &item[key] == name)?
        .get(id)?
        .as_str()
        .map(str::to_owned)
}

/// What the layout's keys stand for on the app: `{track.<key>}`,
/// `{artist.<key>}` and `{album.<key>}`, the album being the one that holds
/// the first track listed for it.
fn library_refs(
    tracks: &Value,
    artists: &Value,
    (name, id, album_id): (&str, &str, &str),
) -> BTreeMap<String, String> {
    let layout = read_json(&reference_dir().join("library.json"));
    let mut refs = BTreeMap::new();
    for track in layout["tracks"].as_array().expect("tracks") {
        let key = track["key"].as_str().expect("track key");
        let found = id_of(tracks, name, &track["title"], id)
            .unwrap_or_else(|| panic!("the app has no track {}", track["title"]));
        refs.insert(format!("track.{key}"), found);
        let album = format!("album.{}", track["album"].as_str().expect("album key"));
        refs.entry(album).or_insert_with(|| {
            id_of(tracks, name, &track["title"], album_id)
                .unwrap_or_else(|| panic!("{} is in no album", track["title"]))
        });
    }
    for artist in layout["artists"].as_array().expect("artists") {
        // An artist the app does not list leaves its placeholder unfilled,
        // and the requests naming it show up as differences.
        if let Some(found) = id_of(artists, "name", &artist["name"], "id")
            .or_else(|| id_of(artists, "Name", &artist["name"], "Id"))
        {
            let key = artist["key"].as_str().expect("artist key");
            refs.insert(format!("artist.{key}"), found);
        }
    }
    refs
}

/// What the layout's keys stand for on the app's Subsonic side.
async fn navidrome_refs(rig: &CompatApp) -> BTreeMap<String, String> {
    let auth = rig.subsonic_auth("dn-ref");
    let (_, everything) = rig
        .app
        .call(
            "GET",
            &format!(
                "/subsonic/rest/search3?query=&artistCount=500&albumCount=500&songCount=500&{auth}"
            ),
            &[],
            None,
        )
        .await;
    let everything = &everything["subsonic-response"]["searchResult3"];
    library_refs(
        &everything["song"],
        &everything["artist"],
        ("title", "id", "albumId"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compat_answers_have_the_shape_navidrome_returns() {
    let rig = CompatApp::start("compat-reference-navidrome").await;
    let mut comparison = Comparison {
        rig: &rig,
        server: "navidrome",
        refs: navidrome_refs(&rig).await,
        found: Found::new(),
    };
    comparison.replay("navidrome-0.62.0", "").await;
    comparison
        .replay("navidrome-0.62.0-writes", "after-writes/")
        .await;
    comparison
        .replay("navidrome-0.62.0-arpeggi", "arpeggi/")
        .await;
    // The ratings were recorded on a fresh server, so a fresh app answers.
    let fresh = CompatApp::start("compat-reference-navidrome-ratings").await;
    comparison.rig = &fresh;
    comparison.refs = navidrome_refs(&fresh).await;
    comparison
        .replay("navidrome-0.62.0-ratings", "ratings/")
        .await;
    comparison.settle();
    fresh.stop().await;
    rig.stop().await;
}

/// What the layout's keys and the caller stand for on the app's Jellyfin
/// side.
async fn jellyfin_refs(rig: &CompatApp) -> BTreeMap<String, String> {
    let auth = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", auth.as_str())];
    let get = |uri: &'static str| rig.app.call("GET", uri, &auth, None);
    let (_, tracks) = get("/jellyfin/Items?Recursive=true&IncludeItemTypes=Audio").await;
    let (_, artists) = get("/jellyfin/Artists").await;
    // Various Artists leads the compilation and is credited on no track.
    let (_, album_artists) = get("/jellyfin/Artists/AlbumArtists").await;
    let (_, views) = get("/jellyfin/UserViews").await;
    let every: Vec<Value> = [&artists["Items"], &album_artists["Items"]]
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
        .cloned()
        .collect();
    let mut refs = library_refs(
        &tracks["Items"],
        &Value::Array(every),
        ("Name", "Id", "AlbumId"),
    );
    refs.insert("user".to_owned(), rig.user_id.clone());
    refs.insert("user_name".to_owned(), PLAYER.to_owned());
    refs.insert("password".to_owned(), rig.secret.clone());
    refs.insert(
        "library".to_owned(),
        views["Items"][0]["Id"]
            .as_str()
            .expect("a music library view")
            .to_owned(),
    );
    refs
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compat_answers_have_the_shape_jellyfin_returns() {
    let rig = CompatApp::start("compat-reference-jellyfin").await;
    let mut comparison = Comparison {
        rig: &rig,
        server: "jellyfin",
        refs: jellyfin_refs(&rig).await,
        found: Found::new(),
    };
    comparison.replay("jellyfin-10.10.6", "").await;
    comparison
        .replay("jellyfin-10.10.6-writes", "after-writes/")
        .await;
    // Lyrics were recorded on a fresh server with a `.lrc` file beside
    // Airbag. A real Jellyfin has no online lookup, so it is off here.
    let lyrics = CompatApp::start_as_recorded(
        "compat-reference-jellyfin-lyrics",
        crate::common::closed_outbound(),
        "jellyfin-10.10.6-lyrics",
    )
    .await;
    lyrics.set_lyrics_lookup(false).await;
    lyrics.wait_for_lyrics_mark("Airbag").await;
    comparison.rig = &lyrics;
    comparison.refs = jellyfin_refs(&lyrics).await;
    comparison
        .replay("jellyfin-10.10.6-lyrics", "lyrics/")
        .await;
    // Played marks were recorded on a fresh server too.
    let played = CompatApp::start("compat-reference-jellyfin-played").await;
    comparison.rig = &played;
    comparison.refs = jellyfin_refs(&played).await;
    comparison
        .replay("jellyfin-10.10.6-played", "played/")
        .await;
    // Paging reads only, so the first app answers it.
    comparison.rig = &rig;
    comparison.refs = jellyfin_refs(&rig).await;
    comparison
        .replay("jellyfin-10.10.6-paging", "paging/")
        .await;
    comparison.settle();
    played.stop().await;
    lyrics.stop().await;
    rig.stop().await;
}

#[test]
fn compat_shape_rules() {
    let differences = |real: Value, ours: Value| -> Vec<String> {
        shape_differences(&real, &ours)
            .into_iter()
            .map(|(path, kind, _, _)| format!("{path} {kind}"))
            .collect()
    };
    // Keys only the app returns are ignored, null on the real side is
    // optional, and an integer passes for a float.
    assert!(
        differences(
            json!({"a": 1.5, "b": null, "c": [{"d": "x"}, {"d": null}]}),
            json!({"a": 1, "c": [{"d": "y"}], "extra": true}),
        )
        .is_empty()
    );
    assert_eq!(
        differences(json!({"a": 1, "b": "x"}), json!({"a": 1.5})),
        ["body.a type", "body.b missing"]
    );
    // A missing object is one difference, not one per key inside it.
    assert_eq!(
        differences(json!({"a": {"b": 1, "c": [2]}}), json!({})),
        ["body.a missing"]
    );
    let short = shape_differences(&json!({"song": [{"id": "1"}]}), &json!({"song": []}));
    assert_eq!(short.len(), 1);
    assert_eq!((short[0].0.as_str(), short[0].1), ("body.song", "empty"));
    assert!(short[0].3.contains("the seed is short"), "{}", short[0].3);

    assert_eq!(
        place("body.subsonic-response.album.song[].bpm", "missing"),
        ("song".to_owned(), "bpm".to_owned(), "missing".to_owned())
    );
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("folder lists") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Keys whose string value is a credential, wherever they sit.
fn unscrubbed(value: &Value, found: &mut Vec<String>) {
    const TOKEN_KEYS: [&str; 8] = [
        "accesstoken",
        "token",
        "api_key",
        "apikey",
        "subsonictoken",
        "subsonicsalt",
        "pw",
        "password",
    ];
    match value {
        Value::Array(items) => items.iter().for_each(|item| unscrubbed(item, found)),
        Value::Object(map) => {
            for (key, inner) in map {
                if let Value::String(text) = inner
                    && TOKEN_KEYS.contains(&key.to_ascii_lowercase().as_str())
                    && !["<token>", "{password}", "wrong-password"].contains(&text.as_str())
                {
                    found.push(key.clone());
                }
                unscrubbed(inner, found);
            }
        }
        _ => {}
    }
}

/// The recorder's own sweep, repeated: `(what, pattern, allowed)`, where a
/// match is a problem unless what follows it starts with an allowed value.
fn private_values(text: &str) -> Vec<&'static str> {
    let rules: [(&str, &str, &[&str]); 6] = [
        ("a JWT", "eyJ", &[]),
        ("a host path", "/(srv|home|tmp)/", &[]),
        ("a password", "[?&]p=", &["***", "wrong-password"]),
        ("a token", "(?i)(api_key|apikey)=", &["<token>", "{token}"]),
        ("a token", "Token=\\\\?\"", &["<token>", "{token}"]),
        ("an IPv4 address", r"\d{1,3}(\.\d{1,3}){3}", &[]),
    ];
    let mut found = Vec::new();
    for (what, pattern, allowed) in rules {
        let pattern = regex::Regex::new(pattern).expect("pattern");
        let hit = pattern.find_iter(text).any(|hit| {
            if what == "an IPv4 address" {
                // Four numbers standing alone, not a slice of a longer run.
                let edge = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_digit() && c != '.');
                return edge(text[..hit.start()].chars().next_back())
                    && edge(text[hit.end()..].chars().next());
            }
            !allowed
                .iter()
                .any(|value| text[hit.end()..].starts_with(value))
        });
        if hit {
            found.push(what);
        }
    }
    found
}

#[test]
fn compat_reference_recordings_are_unchanged_and_hold_nothing_private() {
    assert_eq!(
        private_values("http://10.0.0.12:4533/rest/ping?u=a&p=hunter2"),
        ["a password", "an IPv4 address"]
    );
    assert!(private_values("0.62.0 (1b46b977) ?p=***&api_key=<token>").is_empty());

    let root = reference_dir();
    let mut problems = Vec::new();
    // The hand-written known differences sit beside the recordings and
    // quote them, so they are swept the same way.
    let known = known_differences_dir();
    let swept = files_under(&root)
        .into_iter()
        .map(|file| {
            (
                file.clone(),
                file.strip_prefix(&root).map(Path::to_path_buf),
            )
        })
        .chain(files_under(&known).into_iter().map(|file| {
            let name = file
                .strip_prefix(&known)
                .map(|name| Path::new("known_differences").join(name));
            (file, name)
        }));
    for (file, name) in swept {
        let name = name.expect("under its root").display().to_string();
        let text = std::fs::read_to_string(&file).expect("recording reads as text");
        for what in private_values(&text) {
            problems.push(format!("{name}: holds {what}"));
        }
        if name.ends_with(".json") {
            let mut keys = Vec::new();
            unscrubbed(&read_json(&file), &mut keys);
            for key in keys {
                problems.push(format!("{name}: {key} is not scrubbed"));
            }
        }
    }

    let sha256 = |path: &Path| -> String {
        Sha256::digest(std::fs::read(path).expect("file reads"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let library = sha256(&root.join("library.json"));
    let mut folders = 0;
    for entry in std::fs::read_dir(&root).expect("root lists") {
        let dir = entry.expect("entry").path();
        if !dir.is_dir() {
            continue;
        }
        folders += 1;
        let folder = dir
            .file_name()
            .expect("folder name")
            .to_string_lossy()
            .into_owned();
        let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).expect("checksums read");
        let listed: Map<String, Value> = sums
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (sum, name) = line.split_once("  ").expect("checksum line");
                (name.to_owned(), json!(sum))
            })
            .collect();
        let present: BTreeSet<String> = files_under(&dir)
            .iter()
            .map(|path| {
                path.strip_prefix(&dir)
                    .expect("under the folder")
                    .display()
                    .to_string()
            })
            .filter(|name| name != "SHA256SUMS")
            .collect();
        for name in &present {
            match listed.get(name) {
                None => problems.push(format!("{folder}/{name}: not in SHA256SUMS")),
                Some(sum) if sum != &json!(sha256(&dir.join(name))) => {
                    problems.push(format!("{folder}/{name}: changed since it was recorded"));
                }
                Some(_) => {}
            }
        }
        for name in listed.keys() {
            if !present.contains(name) {
                problems.push(format!("{folder}/{name}: missing"));
            }
        }
        if read_json(&dir.join("capture.json"))["library_sha256"] != json!(library) {
            problems.push(format!(
                "{folder}: library.json changed since it was recorded"
            ));
        }
        let index: Index =
            serde_json::from_value(read_json(&dir.join("index.json"))).expect("index parses");
        for row in index.requests {
            if !listed.contains_key(&row.file) {
                problems.push(format!(
                    "{folder}: index.json names {}, which is missing",
                    row.file
                ));
            }
        }
    }
    assert_eq!(folders, 9, "the recorded folders");
    assert!(
        problems.is_empty(),
        "the reference recordings are read-only:\n{}",
        problems.join("\n")
    );
}
