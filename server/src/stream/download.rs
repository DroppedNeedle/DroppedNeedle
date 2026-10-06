//! Local file downloads: one track as-is, or an album as a ZIP.
//!
//! # Routes (relative; `MediaSetup` nests these under `/api/v3`)
//!
//! ```text
//! GET /download/access                     may the caller download?
//! GET /download/local/track/{id}           one track file
//! GET /download/local/album/{id}           album ZIP by local album id
//! GET /download/local/album/mbid/{mbid}    album ZIP by MusicBrainz id
//! ```
//!
//! Every route needs a session. Who may download is the admin's
//! `security_settings.library_download_access` (everyone, trusted, or
//! admins), read on every request so a change applies at once; refusals
//! are 403 with v2's message.
//!
//! Only catalog files are served (see [`super::local_files`], which
//! playback shares): ids pick catalog rows, paths come from the row and
//! must resolve inside a configured library root. A track download honours
//! a single `Range`, framed by the same code as streaming. Album archives are streamed (see [`archive`]): stored entries,
//! exact `Content-Length`, file bytes read from disk as the client reads,
//! never buffered whole in memory or written to a temp file. Files that
//! cannot be served are left out of an album with a warning, as v2 did;
//! an album with nothing left to serve is 404.

mod archive;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequestParts, Path as UrlPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{StreamExt as _, stream};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::gateway::OUTSIDE_MESSAGE;
use super::local_files::{AlbumFiles, CatalogFile, LibraryFiles, LocateError, PathRefusal};
use super::routes::{ChunkStream, DirectSpan, content_type_for_extension, file_range};
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::UsersDeps;
use crate::auth::users::roles::Role;
use crate::error::{ErrorBody, ErrorEnvelope};
use crate::ids::IdGenerator;
use crate::runtime_config::sections::SecuritySettings;

use crate::library::scan::roots::RootRegistry;

use archive::{Entry, Layout};

/// v2's refusal text when the access setting excludes the caller's role.
const RESTRICTED_MESSAGE: &str = "Library downloads are restricted by the administrator";

/// Reads the security section, per call.
pub type AccessSource = Arc<dyn Fn() -> Result<SecuritySettings, String> + Send + Sync>;

/// Whether the caller may download library files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryDownloadAccess {
    /// True when the access setting admits the caller's role.
    pub allowed: bool,
}

/// Everything the download routes read.
#[derive(Clone)]
pub struct DownloadState {
    library: Option<LibraryFiles>,
    access: AccessSource,
    users: UsersDeps,
    ids: Arc<dyn IdGenerator>,
}

impl DownloadState {
    /// Downloads from the library catalog and roots. `None` (unwired
    /// builds) serves nothing.
    pub fn new(
        library: Option<LibraryFiles>,
        access: AccessSource,
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            library,
            access,
            users,
            ids,
        }
    }
}

/// The security section from a config store, read per call.
pub fn access_from_config(config: Arc<crate::runtime_config::ConfigStore>) -> AccessSource {
    Arc::new(move || {
        config
            .get::<SecuritySettings>()
            .map_err(|error| error.to_string())
    })
}

/// Relative-path router for the `/api/v3` nest inside the session gate.
pub fn download_routes(state: DownloadState) -> Router {
    Router::new()
        .route("/download/access", get(download_access))
        .route("/download/local/track/{id}", get(download_track))
        .route("/download/local/album/{id}", get(download_album))
        .route(
            "/download/local/album/mbid/{mbid}",
            get(download_album_by_mbid),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure a download route answers with.
#[derive(Debug)]
pub enum DownloadError {
    /// No session, or the account is gone.
    Unauthorized,
    /// The access setting excludes the caller, or a path left the roots.
    Forbidden(&'static str),
    /// Unknown id, or nothing on disk to serve.
    NotFound(&'static str),
    /// A `Range` the file cannot meet: 416 with `Content-Range: bytes */N`.
    Unsatisfiable {
        /// File length.
        total_len: u64,
    },
    /// Server fault; the cause is logged with this id.
    Internal {
        /// Ties the response to the log line.
        error_id: String,
    },
}

impl DownloadError {
    fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "library download failed");
        Self::Internal { error_id }
    }
}

impl IntoResponse for DownloadError {
    fn into_response(self) -> Response {
        let content_range = match &self {
            Self::Unsatisfiable { total_len } => {
                HeaderValue::from_str(&format!("bytes */{total_len}")).ok()
            }
            _ => None,
        };
        let (status, code, message, details) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                crate::error::UNAUTHORIZED,
                "Authentication required".to_owned(),
                None,
            ),
            Self::Forbidden(message) => (
                StatusCode::FORBIDDEN,
                crate::error::FORBIDDEN,
                message.to_owned(),
                None,
            ),
            Self::NotFound(message) => (
                StatusCode::NOT_FOUND,
                crate::error::NOT_FOUND,
                message.to_owned(),
                None,
            ),
            Self::Unsatisfiable { .. } => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                super::routes::RANGE_NOT_SATISFIABLE,
                "Range not satisfiable".to_owned(),
                None,
            ),
            Self::Internal { error_id } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::error::INTERNAL_ERROR,
                crate::error::FIXED_INTERNAL_MESSAGE.to_owned(),
                Some(json!({ "error_id": error_id })),
            ),
        };
        let envelope = ErrorEnvelope {
            error: ErrorBody {
                code: code.to_owned(),
                message,
                details,
            },
        };
        let mut response = (status, Json(envelope)).into_response();
        if let Some(value) = content_range {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static(super::routes::WWW_AUTHENTICATE_BEARER),
            );
        }
        response
    }
}

// ---------------------------------------------------------------------------
// Caller
// ---------------------------------------------------------------------------

/// The signed-in caller's role, read fresh from the user store so a role
/// change applies to the next download.
pub struct DownloadUser(Role);

impl FromRequestParts<DownloadState> for DownloadUser {
    type Rejection = DownloadError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DownloadState,
    ) -> Result<Self, Self::Rejection> {
        let session = parts
            .extensions
            .get::<CurrentSession>()
            .ok_or(DownloadError::Unauthorized)?;
        let user = state
            .users
            .users
            .get_by_id(&session.user_id)
            .await
            .map_err(|error| DownloadError::internal(&format!("{error:?}"), state.ids.as_ref()))?
            .ok_or(DownloadError::Unauthorized)?;
        Ok(Self(user.role))
    }
}

/// Whether the access setting admits `role`.
fn allowed(state: &DownloadState, role: Role) -> Result<bool, DownloadError> {
    (state.access)()
        .map(|security| security.download_allowed(role.as_str()))
        .map_err(|cause| DownloadError::internal(&cause, state.ids.as_ref()))
}

fn require_allowed(state: &DownloadState, role: Role) -> Result<(), DownloadError> {
    if allowed(state, role)? {
        Ok(())
    } else {
        Err(DownloadError::Forbidden(RESTRICTED_MESSAGE))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Whether the caller may download library files (for download buttons
/// shown before any album is open).
#[utoipa::path(
    get,
    path = "/api/v3/download/access",
    responses(
        (status = 200, description = "Download capability", body = LibraryDownloadAccess),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn download_access(
    State(state): State<DownloadState>,
    DownloadUser(role): DownloadUser,
) -> Result<Json<LibraryDownloadAccess>, DownloadError> {
    Ok(Json(LibraryDownloadAccess {
        allowed: allowed(&state, role)?,
    }))
}

/// One track file, unchanged, as an attachment. A single `Range` is
/// answered with 206, as v2's file response did, so download managers can
/// resume.
#[utoipa::path(
    get,
    path = "/api/v3/download/local/track/{id}",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "Track file", content_type = "application/octet-stream"),
        (status = 206, description = "The asked byte range of the track file", content_type = "application/octet-stream"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Downloads restricted, or the file left the library roots"),
        (status = 404, description = "Unknown track, or the file is gone"),
        (status = 416, description = "Range not satisfiable"),
    )
)]
pub async fn download_track(
    State(state): State<DownloadState>,
    DownloadUser(role): DownloadUser,
    UrlPath(id): UrlPath<String>,
    request_headers: HeaderMap,
) -> Result<Response, DownloadError> {
    require_allowed(&state, role)?;
    let library = state
        .library
        .as_ref()
        .ok_or(DownloadError::NotFound("Track file not found"))?;
    let (_, path) = library
        .locate_track(&id)
        .await
        .map_err(|error| match error {
            LocateError::Unknown => DownloadError::NotFound("Track file not found"),
            LocateError::Refused(PathRefusal::Missing) => {
                DownloadError::NotFound("Track file not found on disk")
            }
            LocateError::Refused(PathRefusal::Outside) => {
                tracing::warn!(track_id = %id, "download refused: file resolves outside the library roots");
                DownloadError::Forbidden(OUTSIDE_MESSAGE)
            }
            LocateError::Internal(cause) => DownloadError::internal(&cause, state.ids.as_ref()),
        })?;
    let probe = path.clone();
    let size = tokio::task::spawn_blocking(move || std::fs::metadata(probe))
        .await
        .map_err(|cause| DownloadError::internal(&cause, state.ids.as_ref()))?
        .map_err(|_| DownloadError::NotFound("Track file not found on disk"))?
        .len();
    let span = DirectSpan::for_request(request_headers.get(header::RANGE), size)
        .ok_or(DownloadError::Unsatisfiable { total_len: size })?;
    let content_type = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| content_type_for_extension(&ext.to_lowercase()))
        .unwrap_or("application/octet-stream");
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "track".to_owned());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    attachment_headers(&mut headers, &filename);
    span.write_headers(&mut headers);
    Ok((
        span.status(),
        headers,
        Body::from_stream(file_range(path, span.start, span.len)),
    )
        .into_response())
}

/// An album's streamable files as a ZIP, by local album id.
#[utoipa::path(
    get,
    path = "/api/v3/download/local/album/{id}",
    params(("id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "Album archive", content_type = "application/zip"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Downloads restricted"),
        (status = 404, description = "Unknown album, or no files left to archive"),
    )
)]
pub async fn download_album(
    State(state): State<DownloadState>,
    DownloadUser(role): DownloadUser,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, DownloadError> {
    require_allowed(&state, role)?;
    let library = state
        .library
        .as_ref()
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    album_archive(&state, library, &id).await
}

/// An album's streamable files as a ZIP, by MusicBrainz release-group (or
/// release) id; the oldest local copy answers when several exist.
#[utoipa::path(
    get,
    path = "/api/v3/download/local/album/mbid/{mbid}",
    params(("mbid" = String, Path, description = "Release-group or release mbid")),
    responses(
        (status = 200, description = "Album archive", content_type = "application/zip"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Downloads restricted"),
        (status = 404, description = "No local album holds the id, or no files left"),
    )
)]
pub async fn download_album_by_mbid(
    State(state): State<DownloadState>,
    DownloadUser(role): DownloadUser,
    UrlPath(mbid): UrlPath<String>,
) -> Result<Response, DownloadError> {
    require_allowed(&state, role)?;
    let library = state
        .library
        .as_ref()
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    let album_id = library
        .catalog()
        .album_by_mbid(&mbid)
        .await
        .map_err(|cause| DownloadError::internal(&cause, state.ids.as_ref()))?
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    album_archive(&state, library, &album_id).await
}

// ---------------------------------------------------------------------------
// Files on disk
// ---------------------------------------------------------------------------

/// Canonical path, size and modification time of one catalog file.
/// Blocking.
fn locate(registry: &RootRegistry, file: &CatalogFile) -> Result<(PathBuf, u64, i64), PathRefusal> {
    let path = super::local_files::resolve(registry, file)?;
    let metadata = std::fs::metadata(&path).map_err(|_| PathRefusal::Missing)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |span| i64::try_from(span.as_secs()).unwrap_or(i64::MAX));
    Ok((path, metadata.len(), modified))
}

/// One archive member: its entry and where its bytes live.
struct Member {
    entry: Entry,
    path: PathBuf,
}

/// Plan, then stream, one album archive.
async fn album_archive(
    state: &DownloadState,
    library: &LibraryFiles,
    album_id: &str,
) -> Result<Response, DownloadError> {
    let album = library
        .catalog()
        .album(album_id)
        .await
        .map_err(|cause| DownloadError::internal(&cause, state.ids.as_ref()))?
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    let library = library.clone();
    let album_id = album_id.to_owned();
    let (archive_name, members) =
        tokio::task::spawn_blocking(move || plan_members(&library.registry(), &album_id, album))
            .await
            .map_err(|cause| DownloadError::internal(&cause, state.ids.as_ref()))?;
    if members.is_empty() {
        return Err(DownloadError::NotFound("Album or track files not found"));
    }
    let entries: Vec<Entry> = members.iter().map(|member| member.entry.clone()).collect();
    let layout = archive::layout(&entries);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    attachment_headers(&mut headers, &archive_name);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(layout.total_len));
    let body = Body::from_stream(archive_stream(members, entries, layout));
    Ok((StatusCode::OK, headers, body).into_response())
}

/// Resolve every file of the album, leaving out (with a warning) any that
/// cannot be served, and name the entries the way v2 did:
/// `NN Title.ext`, or `DD-NN Title.ext` when the album spans discs.
/// Blocking.
fn plan_members(
    registry: &RootRegistry,
    album_id: &str,
    album: AlbumFiles,
) -> (String, Vec<Member>) {
    let archive_name = sanitize_filename(&format!("{} - {}.zip", album.artist_name, album.title));
    let mut located = Vec::new();
    for file in &album.files {
        match locate(registry, file) {
            Ok(found) => located.push((file, found)),
            Err(refusal) => tracing::warn!(
                album_id,
                track_id = %file.track_id,
                ?refusal,
                "left a track out of the album archive"
            ),
        }
    }
    let discs: std::collections::HashSet<i64> =
        located.iter().map(|(file, _)| file.disc_number).collect();
    let multi_disc = discs.len() > 1;
    let mut taken = std::collections::HashSet::new();
    let members = located
        .into_iter()
        .map(|(file, (path, size, modified))| {
            let name = unique_name(&entry_name(file, &path, multi_disc), &mut taken);
            let (dos_time, dos_date) = archive::dos_datetime(modified);
            Member {
                entry: Entry {
                    name,
                    size,
                    dos_time,
                    dos_date,
                },
                path,
            }
        })
        .collect();
    (archive_name, members)
}

fn entry_name(file: &CatalogFile, path: &Path, multi_disc: bool) -> String {
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let title = sanitize_filename(&file.title);
    if multi_disc {
        format!(
            "{:02}-{:02} {title}{extension}",
            file.disc_number, file.track_number
        )
    } else {
        format!("{:02} {title}{extension}", file.track_number)
    }
}

/// Two tracks can share a number and title (a bonus disc tagged as disc 1,
/// say); the second becomes `Name (2).ext` instead of shadowing the first
/// when unzipped. v2 wrote both under one name.
fn unique_name(name: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if taken.insert(name.to_lowercase()) {
        return name.to_owned();
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) => name.split_at(dot),
        None => (name, ""),
    };
    (2u32..)
        .map(|copy| format!("{stem} ({copy}){extension}"))
        .find(|candidate| taken.insert(candidate.to_lowercase()))
        .unwrap_or_else(|| name.to_owned())
}

/// Read a whole file once for its CRC-32, checking it still has the size
/// the archive was planned with.
async fn file_crc(path: PathBuf, size: u64) -> std::io::Result<u32> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut file = std::fs::File::open(&path)?;
        let mut crc = flate2::Crc::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            crc.update(&buffer[..read]);
            total += read as u64;
        }
        if total != size {
            return Err(std::io::Error::other(
                "file changed size while the album archive was streaming",
            ));
        }
        Ok(crc.sum())
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// The archive as a byte stream: per member, its CRC pass, local header and
/// file bytes; then the central directory. A file that fails mid-way ends
/// the stream with an error, so the client sees a broken download, never a
/// silently short archive.
fn archive_stream(members: Vec<Member>, entries: Vec<Entry>, layout: Layout) -> ChunkStream {
    let crcs = Arc::new(Mutex::new(Vec::with_capacity(members.len())));
    let recorded = Arc::clone(&crcs);
    let bodies = stream::iter(members)
        .then(move |member| {
            let crcs = Arc::clone(&recorded);
            async move {
                let crc = file_crc(member.path.clone(), member.entry.size).await?;
                crcs.lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(crc);
                Ok::<_, std::io::Error>((archive::local_header(&member.entry, crc), member))
            }
        })
        .flat_map(|prepared| match prepared {
            Ok((header, member)) => stream::once(async move { Ok(header) })
                .chain(file_range(member.path, 0, member.entry.size))
                .boxed(),
            Err(error) => {
                tracing::warn!(%error, "album archive stopped mid-stream");
                stream::once(async move { Err(error) }).boxed()
            }
        });
    let tail = stream::once(async move {
        let crcs = crcs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if crcs.len() != entries.len() {
            return Err(std::io::Error::other("album archive lost a member"));
        }
        Ok(archive::central_directory(&entries, &crcs, &layout))
    });
    bodies.chain(tail).boxed()
}

// ---------------------------------------------------------------------------
// Names and headers
// ---------------------------------------------------------------------------

/// Replace characters no common filesystem accepts (v2
/// `sanitize_filename`); a blank result reads `Untitled`.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `Content-Disposition: attachment` with a plain ASCII fallback name and
/// the exact UTF-8 name (RFC 6266).
fn attachment_headers(headers: &mut HeaderMap, filename: &str) {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    )) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names_follow_v2_and_never_collide() {
        let mut taken = std::collections::HashSet::new();
        assert_eq!(unique_name("01 Song.flac", &mut taken), "01 Song.flac");
        assert_eq!(unique_name("01 Song.flac", &mut taken), "01 Song (2).flac");
        assert_eq!(unique_name("01 SONG.flac", &mut taken), "01 SONG (3).flac");
        assert_eq!(sanitize_filename(" AC/DC: Live? "), "AC_DC_ Live_");
        assert_eq!(sanitize_filename("   "), "Untitled");
    }

    #[test]
    fn disposition_carries_ascii_and_utf8_names() {
        let mut headers = HeaderMap::new();
        attachment_headers(&mut headers, "Sigur Rós - ( ).zip");
        let value = headers
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            value,
            "attachment; filename=\"Sigur R_s - ( ).zip\"; \
             filename*=UTF-8''Sigur%20R%C3%B3s%20-%20%28%20%29.zip"
        );
    }
}
