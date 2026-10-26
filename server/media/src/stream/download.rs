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
    Json,
    body::Body,
    extract::{FromRef, Path as UrlPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt as _, stream};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::gateway::OUTSIDE_MESSAGE;
use super::local_files::{AlbumFiles, CatalogFile, LibraryFiles, LocateError, PathRefusal};
use super::routes::{
    ChunkStream, DirectSpan, content_type_for_extension, file_range, range_header, unsatisfiable,
};
use droppedneedle_auth::{Role, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;

use droppedneedle_library::scan::roots::RootRegistry;

use archive::{Entry, Layout};

/// v2's refusal text when the access setting excludes the caller's role.
const RESTRICTED_MESSAGE: &str = "Library downloads are restricted by the administrator";

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
    access: Live<SecuritySettings>,
}

impl DownloadState {
    /// Downloads from the library catalog and roots. `None` (unwired
    /// builds) serves nothing. The access setting in `access` is read per
    /// call.
    pub fn new(library: Option<LibraryFiles>, access: Live<SecuritySettings>) -> Self {
        Self { library, access }
    }
}

/// The download routes, for any signed-in user.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    DownloadState: FromRef<S>,
{
    Routes::new()
        .user(routes!(download_access))
        .user(routes!(download_track))
        .user(routes!(download_album))
        .user(routes!(download_album_by_mbid))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure a download route answers with.
#[derive(Debug)]
pub enum DownloadError {
    /// The access setting excludes the caller, or a path left the roots.
    Forbidden(&'static str),
    /// Unknown id, or nothing on disk to serve.
    NotFound(&'static str),
    /// A `Range` the file cannot meet: 416 with `Content-Range: bytes */N`.
    Unsatisfiable {
        /// File length.
        total_len: u64,
    },
    /// Server fault. The cause reaches the log only.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

impl DownloadError {
    fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl From<DownloadError> for ApiError {
    fn from(error: DownloadError) -> Self {
        match error {
            DownloadError::Forbidden(message) => ApiError::forbidden(message),
            DownloadError::NotFound(message) => ApiError::new(
                StatusCode::NOT_FOUND,
                droppedneedle_kernel::error::NOT_FOUND,
                message,
            ),
            DownloadError::Unsatisfiable { total_len } => unsatisfiable(total_len),
            DownloadError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

/// Whether the access setting admits `role`.
fn allowed(state: &DownloadState, role: Role) -> Result<bool, DownloadError> {
    state
        .access
        .get()
        .map(|security| security.download_allowed(role.as_str()))
        .map_err(DownloadError::internal)
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
    tag = "stream",
    path = "/api/v3/download/access",
    responses(
        (status = 200, description = "Download capability", body = LibraryDownloadAccess),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn download_access(
    State(state): State<DownloadState>,
    User(user): User,
) -> Result<Json<LibraryDownloadAccess>, ApiError> {
    Ok(Json(LibraryDownloadAccess {
        allowed: allowed(&state, user.role())?,
    }))
}

/// One track file, unchanged, as an attachment. A single `Range` is
/// answered with 206, as v2's file response did, so download managers can
/// resume.
#[utoipa::path(
    get,
    tag = "stream",
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
    User(user): User,
    UrlPath(id): UrlPath<String>,
    request_headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_allowed(&state, user.role())?;
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
            LocateError::Internal(cause) => DownloadError::internal(cause),
        })?;
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|_| DownloadError::NotFound("Track file not found on disk"))?
        .len();
    let span = DirectSpan::for_request(range_header(&request_headers), size)
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
    tag = "stream",
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
    User(user): User,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, ApiError> {
    require_allowed(&state, user.role())?;
    let library = state
        .library
        .as_ref()
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    Ok(album_archive(library, &id).await?)
}

/// An album's streamable files as a ZIP, by MusicBrainz release-group (or
/// release) id; the oldest local copy answers when several exist.
#[utoipa::path(
    get,
    tag = "stream",
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
    User(user): User,
    UrlPath(mbid): UrlPath<String>,
) -> Result<Response, ApiError> {
    require_allowed(&state, user.role())?;
    let library = state
        .library
        .as_ref()
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    let album_id = library
        .catalog()
        .album_by_mbid(&mbid)
        .await
        .map_err(DownloadError::internal)?
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    Ok(album_archive(library, &album_id).await?)
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
async fn album_archive(library: &LibraryFiles, album_id: &str) -> Result<Response, DownloadError> {
    let album = library
        .catalog()
        .album(album_id)
        .await
        .map_err(DownloadError::internal)?
        .ok_or(DownloadError::NotFound("Album or track files not found"))?;
    let library = library.clone();
    let album_id = album_id.to_owned();
    let (archive_name, members) =
        tokio::task::spawn_blocking(move || plan_members(&library.registry(), &album_id, album))
            .await
            .map_err(DownloadError::internal)?;
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
