//! Audio streaming and PlaybackInfo.

use crate::auth::compat_auth::jellyfin::JellyfinPasswordStore;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use uuid::Uuid;

use super::builders::{self};
use super::models::{MediaSourceInfo, PlaybackInfoBody, PlaybackInfoResponse};
use super::params::{self, CiParams};
use super::router::*;
use super::seams::{
    ByteOutcome, DecideInput, IdMap, LibraryRead, PlaybackSessions, StreamEngine, StreamPlan,
    TICKS_PER_SECOND, decide,
};
use crate::compat::subsonic::stream::download_filename;

// ===== Streaming + PlaybackInfo =====

/// Accepted `Container` values: comma-separated, pipe-variants split with
/// the first token winning (v2 `_accepted_containers`).
pub(super) fn accepted_containers(param: Option<&str>) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for entry in param.unwrap_or("").split(',') {
        let entry = entry.trim();
        if !entry.is_empty() {
            out.insert(entry.split('|').next().unwrap_or("").trim().to_lowercase());
        }
    }
    out
}

/// Client codec → what we can produce: `mp3`/`opus` pass through, anything
/// else coerces to `opus`, "nearest we can produce" (v2 `_map_jf_codec`).
pub(super) fn map_codec(codec: Option<&str>) -> Option<String> {
    let codec = codec.unwrap_or("");
    if codec.is_empty() {
        return None;
    }
    let lower = codec.to_lowercase();
    Some(if lower == "mp3" || lower == "opus" {
        lower
    } else {
        "opus".to_owned()
    })
}

/// Decode an audio item id to its file id (v2 `_decode_track`).
pub(super) async fn decode_track<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
) -> Result<String, Response>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    match state.ids.from_jf(item_id).await {
        Some((kind, internal)) if kind == "track" => Ok(internal),
        _ => Err(error(StatusCode::NOT_FOUND)),
    }
}

/// Run the policy and serve direct or transcoded bytes (v2
/// `_stream_decided`, minus the plugin fallback: no plugin seam exists
/// here yet, so a local miss is a plain 404.
pub(super) async fn stream_decided<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    range: Option<&str>,
    internal: &str,
    req_format: Option<String>,
    max_kbps: Option<u32>,
    start_seconds: f64,
    force: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let Some(track) = state.library.track("", internal).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let settings = state.settings.jellyfin();
    let plan = decide(&DecideInput {
        src_format: track.file_format.as_deref(),
        src_bitrate_kbps: track.bitrate.unwrap_or(0),
        requested: req_format.as_deref(),
        ceiling_kbps: max_kbps,
        force_original: force,
        start_seconds,
        transcoding_enabled: settings.transcoding_enabled,
        server_max_kbps: settings.transcode_max_bitrate_kbps,
        default_format: &settings.transcode_default_format,
        ffmpeg: settings.ffmpeg_available,
    });
    match plan {
        StreamPlan::Direct => outcome_response(&state.engine.direct(internal, range).await),
        StreamPlan::Transcode {
            format,
            bitrate_kbps,
            start_seconds,
        } => outcome_response(
            &state
                .engine
                .transcode(internal, &format, bitrate_kbps, start_seconds)
                .await,
        ),
    }
}

pub(super) async fn audio<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, tail)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    // Owned inputs only past this point: `&Request<Body>` is not `Send`, so
    // holding it across an await would break the `Handler` impl.
    let headers = request.headers().clone();
    let query = request.uri().query().map(str::to_owned);
    let caller = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(caller) => caller,
        Err(denied) => return denied,
    };
    // Leases count against the caller, like the native stream routes.
    let mut state = state;
    state.engine = state.engine.for_caller(&caller.principal.id);
    // Tails match case-insensitively: Finamp asks for `Universal`.
    let tail = tail.to_ascii_lowercase();
    match tail.as_str() {
        "universal" => universal(&state, &headers, query.as_deref(), &item_id).await,
        HLS_PLAYLIST => hls_playlist(&state, query.as_deref(), &item_id).await,
        HLS_SEGMENT => hls_segment(&state, query.as_deref(), &item_id).await,
        _ if is_stream_tail(&tail) => {
            audio_stream(&state, &headers, query.as_deref(), &item_id).await
        }
        _ => error(StatusCode::NOT_FOUND),
    }
}

/// `stream` or `stream.<ext>`, already lowercased.
fn is_stream_tail(tail: &str) -> bool {
    tail == "stream" || tail.starts_with("stream.")
}

/// HLS playlist tail (Finamp's transcoded playback).
const HLS_PLAYLIST: &str = "main.m3u8";
/// The playlist's one segment, relative to the playlist URL.
const HLS_SEGMENT: &str = "main.ts";

/// `GET /Audio/{id}/main.m3u8`: an HLS VOD playlist with a single segment
/// covering the whole track. Finamp plays transcodes through HLS; one
/// segment keeps the track gapless (no per-segment encoder priming) and
/// needs no segment cache. The segment URL carries the request's query, so
/// `ApiKey` and `audioBitRate` reach it. Without ffmpeg, or with
/// transcoding off, there is nothing to serve: 404.
pub(super) async fn hls_playlist<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let settings = state.settings.jellyfin();
    if !settings.transcoding_enabled || !settings.ffmpeg_available {
        return error(StatusCode::NOT_FOUND);
    }
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let Some(duration) = state
        .library
        .track("", &internal)
        .await
        .and_then(|track| track.duration_seconds)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    else {
        return error(StatusCode::NOT_FOUND);
    };
    let segment = match query.filter(|query| !query.is_empty()) {
        Some(query) => format!("{HLS_SEGMENT}?{query}"),
        None => HLS_SEGMENT.to_owned(),
    };
    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXTINF:{duration:.6},\n{segment}\n#EXT-X-ENDLIST\n",
        duration.ceil() as u64
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-store")
        .body(Body::from(playlist))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

/// `GET /Audio/{id}/main.ts`: the playlist's segment, the whole track as
/// AAC in MPEG-TS at the client's `audioBitRate` (bits per second), else
/// the AAC default, capped by the server ceiling.
pub(super) async fn hls_segment<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    use crate::stream::transcode::{HLS_SEGMENT_FORMAT, out_bitrate_kbps};

    let settings = state.settings.jellyfin();
    if !settings.transcoding_enabled || !settings.ffmpeg_available {
        return error(StatusCode::NOT_FOUND);
    }
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let audio_bps = params::qint(&q, "audioBitRate", 0);
    let cap_kbps = (audio_bps > 0).then(|| (audio_bps as f64 / 1000.0).round() as i64);
    let bitrate = out_bitrate_kbps(
        HLS_SEGMENT_FORMAT,
        cap_kbps,
        i64::from(settings.transcode_max_bitrate_kbps),
    );
    outcome_response(
        &state
            .engine
            .transcode(
                &internal,
                HLS_SEGMENT_FORMAT,
                u32::try_from(bitrate).unwrap_or(u32::MAX),
                0.0,
            )
            .await,
    )
}

/// `GET /Items/{id}/File`: the original file with Range support. Finamp
/// plays the tracks it does not transcode from it, so it asks for sign-in
/// only and never reads the download setting, as in Jellyfin 10.10.6
/// (`GetFile` is plain `[Authorize]`).
pub(super) async fn item_file<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (headers, query) = auth_inputs(&request);
    original_file(
        &state,
        &headers,
        query.as_deref(),
        &item_id,
        Original::Play,
        false,
    )
    .await
}

/// `HEAD /Items/{id}/File`: the headers GET would answer, no body, no lease.
pub(super) async fn item_file_head<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (headers, query) = auth_inputs(&request);
    original_file(
        &state,
        &headers,
        query.as_deref(),
        &item_id,
        Original::Play,
        true,
    )
    .await
}

/// `GET /Items/{id}/Download`: the original file as an attachment, for a
/// user the admin's download setting admits.
///
/// Read in the Jellyfin 10.10.6 source on 2026-10-14 (`GetDownload` in
/// `LibraryController.cs`; there is no recording of this route): it
/// carries `Policies.Download`, so a signed-in user without
/// `EnableContentDownloading` gets 403 before the item is looked up, an
/// unknown item is 404, and the file is sent with its own content type,
/// Range support and `Content-Disposition: attachment`. Jellyfin names the
/// attachment after the file on disk. Here it is named after the track's
/// title, as the Subsonic download is, so no name from the host is sent.
/// The header is always sent.
pub(super) async fn item_download<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (headers, query) = auth_inputs(&request);
    original_file(
        &state,
        &headers,
        query.as_deref(),
        &item_id,
        Original::Download,
        false,
    )
    .await
}

/// `HEAD /Items/{id}/Download`: the headers GET would answer, refused for
/// the same users.
pub(super) async fn item_download_head<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (headers, query) = auth_inputs(&request);
    original_file(
        &state,
        &headers,
        query.as_deref(),
        &item_id,
        Original::Download,
        true,
    )
    .await
}

/// What sign-in reads from a request: its headers and its query.
fn auth_inputs(request: &Request<Body>) -> (HeaderMap, Option<String>) {
    (
        request.headers().clone(),
        request.uri().query().map(str::to_owned),
    )
}

/// Why the original file is asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Original {
    /// To play it: any signed-in user.
    Play,
    /// To keep it: only a user the download setting admits.
    Download,
}

/// The original file for both `/Items/{id}/File` and
/// `/Items/{id}/Download`. The download setting is read on every request.
async fn original_file<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
    purpose: Original,
    head: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(state) {
        return denied;
    }
    let caller = match authed(&state.passwords, headers, query).await {
        Ok(caller) => caller.principal,
        Err(denied) => return denied,
    };
    let download = purpose == Original::Download;
    if download
        && !state
            .settings
            .jellyfin()
            .download_access
            .admits(&caller.role)
    {
        return error(StatusCode::FORBIDDEN);
    }
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let range = header(headers, "range");
    let mut outcome = if head {
        state.engine.head(&internal, range).await
    } else {
        let engine = state.engine.for_caller(&caller.id);
        engine.direct(&internal, range).await
    };
    if download && matches!(outcome.status, 200 | 206) {
        // A row deleted since the bytes were opened still downloads, under
        // a plain name.
        let name = match state.library.track(&caller.id, &internal).await {
            Some(track) => {
                download_filename(&track.title, track.file_format.as_deref().unwrap_or(""))
            }
            None => download_filename("track", served_format(&outcome)),
        };
        outcome.headers.push((
            "Content-Disposition".to_owned(),
            format!("attachment; filename=\"{name}\""),
        ));
    }
    if head {
        head_outcome_response(&outcome)
    } else {
        outcome_response(&outcome)
    }
}

/// The format of the bytes an outcome serves, from its content type: the
/// reverse of `content_type_for_format`. Empty when it names none.
fn served_format(outcome: &ByteOutcome) -> &'static str {
    let content_type = outcome
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map_or("", |(_, value)| value.as_str());
    match content_type {
        "audio/flac" => "flac",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" => "wav",
        "audio/opus" => "opus",
        _ => "",
    }
}

/// HEAD for one item: the status and headers a direct GET would answer.
async fn head_response<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    head_outcome_response(&state.engine.head(&internal, header(headers, "range")).await)
}

/// A HEAD outcome as a response: its status and headers, no body.
fn head_outcome_response(outcome: &ByteOutcome) -> Response {
    if outcome.status == StatusCode::NOT_FOUND.as_u16() {
        return error(StatusCode::NOT_FOUND);
    }
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in &outcome.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(Body::empty())
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

/// `HEAD /Audio/...`: the same status and headers GET would answer
/// (200/206/416; HEAD honors Range), always with an empty body, no
/// lease (v2 `_audio_stream_head`).
pub(super) async fn audio_head<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, tail)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    // Unknown tails 404 exactly like GET (no `universal`/`stream` probe
    // that answers 200 for a route GET would refuse). The playlist answers
    // like GET with the body dropped; the HLS segment is a live transcode,
    // so HEAD never starts one.
    let tail = tail.to_ascii_lowercase();
    if tail != "universal" && tail != HLS_PLAYLIST && !is_stream_tail(&tail) {
        return error(StatusCode::NOT_FOUND);
    }
    let headers = request.headers().clone();
    let query = request.uri().query().map(str::to_owned);
    if let Err(denied) = authed(&state.passwords, &headers, query.as_deref()).await {
        return denied;
    }
    if tail == HLS_PLAYLIST {
        let (parts, body) = hls_playlist(&state, query.as_deref(), &item_id)
            .await
            .into_parts();
        let length = axum::body::to_bytes(body, usize::MAX)
            .await
            .map_or(0, |bytes| bytes.len());
        let mut response = Response::from_parts(parts, Body::empty());
        if let Ok(value) = length.to_string().parse() {
            response
                .headers_mut()
                .insert(axum::http::header::CONTENT_LENGTH, value);
        }
        return response;
    }
    head_response(&state, &headers, &item_id).await
}

/// `/universal` container negotiation (v2 `_universal`).
pub(super) async fn universal<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let max_bps = params::qint(&q, "MaxStreamingBitrate", 0);
    let max_kbps = (max_bps > 0).then(|| (max_bps as f64 / 1000.0).round() as u32);
    let start_seconds = params::qint(&q, "StartTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64;
    let accepted = accepted_containers(q.get("Container"));
    let track_format = state
        .library
        .track("", &internal)
        .await
        .and_then(|t| t.file_format);
    let req_format = if track_format
        .as_deref()
        .is_some_and(|f| accepted.contains(&f.to_lowercase()))
    {
        None
    } else {
        map_codec(q.get("AudioCodec"))
    };
    let range = header(headers, "range").map(str::to_owned);
    stream_decided(
        state,
        range.as_deref(),
        &internal,
        req_format,
        max_kbps,
        start_seconds,
        false,
    )
    .await
}

/// `/stream[.ext]`: `static=true` (case-insensitive) forces direct (v2
/// `_audio_stream`).
pub(super) async fn audio_stream<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    if q.get("static").unwrap_or("").eq_ignore_ascii_case("true") {
        let range = header(headers, "range");
        return outcome_response(&state.engine.direct(&internal, range).await);
    }
    let audio_bps = params::qint(&q, "audioBitRate", 0);
    let max_kbps = (audio_bps > 0).then(|| (audio_bps as f64 / 1000.0).round() as u32);
    let start_seconds = params::qint(&q, "startTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64;
    let range = header(headers, "range").map(str::to_owned);
    stream_decided(
        state,
        range.as_deref(),
        &internal,
        map_codec(q.get("audioCodec")),
        max_kbps,
        start_seconds,
        false,
    )
    .await
}

/// GET+POST PlaybackInfo (v2 `_playback_info`). `DirectStreamUrl` embeds
/// `?api_key=<token>` for headerless players; transcoding fields appear only
/// when the policy says transcode; Finamp's 15 non-null fields are always
/// present (MediaStream 5 + MediaSourceInfo 10, v2 issue #438).
pub(super) async fn playback_info<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let is_post = request.method() == axum::http::Method::POST;
    let advertised = local_address(&state, &request);
    let (parts, body) = request.into_parts();
    let authed = match authed(&state.passwords, &parts.headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let internal = match decode_track(&state, &item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let Some(track) = state.library.track(&authed.principal.id, &internal).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let mut max_bps: Option<i64> = None;
    if is_post
        && let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await
        && !bytes.is_empty()
        && let Ok(parsed) = serde_json::from_slice::<PlaybackInfoBody>(&bytes)
        && parsed.max_streaming_bitrate.unwrap_or(0) != 0
    {
        max_bps = parsed.max_streaming_bitrate;
    }
    if max_bps.is_none() {
        let q = CiParams::parse(raw_query.as_deref());
        if let Some(raw) = q.get("maxStreamingBitrate")
            && !raw.is_empty()
            && raw.bytes().all(|b| b.is_ascii_digit())
            && let Ok(parsed) = raw.parse::<i64>()
        {
            max_bps = Some(parsed);
        }
    }
    let max_kbps = max_bps
        .filter(|b| *b > 0)
        .map(|b| (b as f64 / 1000.0).round() as u32);
    let settings = state.settings.jellyfin();
    let will_transcode = matches!(
        decide(&DecideInput {
            src_format: track.file_format.as_deref(),
            src_bitrate_kbps: track.bitrate.unwrap_or(0),
            requested: None,
            ceiling_kbps: max_kbps,
            force_original: false,
            start_seconds: 0.0,
            transcoding_enabled: settings.transcoding_enabled,
            server_max_kbps: settings.transcode_max_bitrate_kbps,
            default_format: &settings.transcode_default_format,
            ffmpeg: settings.ffmpeg_available,
        }),
        StreamPlan::Transcode { .. }
    );
    let psid = Uuid::new_v4().simple().to_string();
    let ext = track
        .file_format
        .clone()
        .unwrap_or_else(|| "dat".to_owned());
    let direct_url = format!(
        "{}/Audio/{item_id}/stream.{ext}?static=true&mediaSourceId={item_id}&api_key={}",
        advertised, authed.principal.token,
    );
    let mut src = MediaSourceInfo {
        id: item_id.clone(),
        protocol: "File".to_owned(),
        container: track.file_format.clone(),
        size: track.file_size_bytes,
        bitrate: track
            .bitrate
            .map(|b| u64::from(b) * 1000)
            .filter(|b| *b != 0),
        run_time_ticks: builders::ticks(track.duration_seconds),
        supports_direct_play: true,
        supports_direct_stream: true,
        supports_transcoding: settings.transcoding_enabled && settings.ffmpeg_available,
        default_audio_stream_index: 0,
        media_streams: vec![builders::media_stream(&track)],
        name: None,
        is_remote: false,
        direct_stream_url: Some(direct_url),
        transcoding_url: None,
        transcoding_sub_protocol: None,
        transcoding_container: None,
        source_type: "Default".to_owned(),
        is_infinite_stream: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: false,
        read_at_native_framerate: false,
        ignore_dts: false,
        ignore_index: false,
        gen_pts_input: false,
    };
    if will_transcode {
        let out = settings.transcode_default_format.clone();
        // Root-relative yet inside the deployment prefix: players resolve
        // against the advertised origin, so a bare path would escape the
        // base path under non-empty BASE_PATH deployments (v2 `_playback_info`).
        src.transcoding_url = Some(format!(
            "{}/jellyfin/Audio/{item_id}/universal?AudioCodec={out}&Container={out}&PlaySessionId={psid}&api_key={}",
            state.listener.base_path, authed.principal.token,
        ));
        src.transcoding_sub_protocol = Some("http".to_owned());
        src.transcoding_container = Some(if out == "mp3" {
            "mp3".to_owned()
        } else {
            "ogg".to_owned()
        });
    }
    json(
        StatusCode::OK,
        &PlaybackInfoResponse {
            media_sources: vec![src],
            play_session_id: psid,
            error_code: None,
        },
    )
}
