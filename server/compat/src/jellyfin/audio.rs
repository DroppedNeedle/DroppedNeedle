//! Audio streaming and PlaybackInfo.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use droppedneedle_kernel::error::ApiError;
use uuid::Uuid;

use super::builders::{self};
use super::models::{PlaybackInfoBody, PlaybackInfoResponse};
use super::params::{self, CiParams};
use super::router::*;
use super::seams::{ByteOutcome, Principal, StreamParams, TICKS_PER_SECOND};
use crate::subsonic::stream::download_filename;
use droppedneedle_media::stream::transcode::{HLS_SEGMENT_FORMAT, TrackInfo, decide};

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
pub(super) async fn decode_track(state: &JellyfinState, item_id: &str) -> Result<String, Response> {
    match state.ids.from_jf(item_id).await {
        Ok(Some((kind, internal))) if kind == "track" => Ok(internal),
        Ok(_) => Err(error(StatusCode::NOT_FOUND)),
        Err(failed) => Err(ApiError::from(failed).into_response()),
    }
}

/// Serve one track as `params` ask; the stream gateway decides direct or
/// transcode (v2 `_stream_decided`). A local miss falls back to a
/// `streaming_source` plugin behind the stream gateway. HEAD answers the
/// status and headers GET would, with no body and no transcode started.
async fn play(
    state: &JellyfinState,
    internal: &str,
    params: &StreamParams,
    range: Option<&str>,
    head: bool,
) -> Response {
    let outcome = state
        .engine
        .play_jellyfin(internal, params, range, head)
        .await;
    if head {
        head_outcome_response(&outcome)
    } else {
        outcome_response(&outcome)
    }
}

/// A client's bits-per-second value as a kbps ceiling; 0 or less is none.
fn ceiling_kbps(bits_per_second: i64) -> Option<i64> {
    (bits_per_second > 0).then(|| (bits_per_second as f64 / 1000.0).round() as i64)
}

pub(super) async fn audio(
    State(state): State<JellyfinState>,
    Path((item_id, tail)): Path<(String, String)>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
    // Owned inputs only past this point: `&Request<Body>` is not `Send`, so
    // holding it across an await would break the `Handler` impl.
    let headers = request.headers().clone();
    let query = request.uri().query().map(str::to_owned);
    // Leases count against the caller, like the native stream routes.
    let mut state = state;
    state.engine = state.engine.for_caller(&caller.principal.id);
    // Tails match case-insensitively: Finamp asks for `Universal`.
    let tail = tail.to_ascii_lowercase();
    match tail.as_str() {
        "lyrics" => {
            let missing = not_found_item(&request);
            super::lyrics::lyrics(&state, &item_id, missing).await
        }
        "universal" => universal(&state, &headers, query.as_deref(), &item_id, false).await,
        HLS_PLAYLIST => hls_playlist(&state, query.as_deref(), &item_id).await,
        HLS_SEGMENT => hls_segment(&state, query.as_deref(), &item_id).await,
        _ if is_stream_tail(&tail) => {
            audio_stream(&state, &headers, query.as_deref(), &item_id, false).await
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
pub(super) async fn hls_playlist(
    state: &JellyfinState,
    query: Option<&str>,
    item_id: &str,
) -> Response {
    let settings = state.settings.jellyfin();
    if !settings.transcoding_enabled || !settings.ffmpeg_available {
        return error(StatusCode::NOT_FOUND);
    }
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let track = match state.library.track("", &internal).await {
        Ok(track) => track,
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    let Some(duration) = track
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
pub(super) async fn hls_segment(
    state: &JellyfinState,
    query: Option<&str>,
    item_id: &str,
) -> Response {
    let settings = state.settings.jellyfin();
    if !settings.transcoding_enabled || !settings.ffmpeg_available {
        return error(StatusCode::NOT_FOUND);
    }
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let params = StreamParams {
        format: Some(HLS_SEGMENT_FORMAT.to_owned()),
        max_bitrate_kbps: ceiling_kbps(params::qint(&q, "audioBitRate", 0)),
        ..StreamParams::default()
    };
    play(state, &internal, &params, None, false).await
}

/// `GET /Items/{id}/File`: the original file with Range support. Finamp
/// plays the tracks it does not transcode from it, so it asks for sign-in
/// only and never reads the download setting, as in Jellyfin 10.10.6
/// (`GetFile` is plain `[Authorize]`).
pub(super) async fn item_file(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
    original_file(
        &state,
        caller.principal,
        request.headers(),
        &item_id,
        Original::Play,
        false,
    )
    .await
}

/// `HEAD /Items/{id}/File`: the headers GET would answer, no body, no lease.
pub(super) async fn item_file_head(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
    original_file(
        &state,
        caller.principal,
        request.headers(),
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
pub(super) async fn item_download(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
    original_file(
        &state,
        caller.principal,
        request.headers(),
        &item_id,
        Original::Download,
        false,
    )
    .await
}

/// `HEAD /Items/{id}/Download`: the headers GET would answer, refused for
/// the same users.
pub(super) async fn item_download_head(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
    original_file(
        &state,
        caller.principal,
        request.headers(),
        &item_id,
        Original::Download,
        true,
    )
    .await
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
async fn original_file(
    state: &JellyfinState,
    caller: Principal,
    headers: &HeaderMap,
    item_id: &str,
    purpose: Original,
    head: bool,
) -> Response {
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
    let mut outcome = state
        .engine
        .for_caller(&caller.id)
        .play_jellyfin(&internal, &StreamParams::original(), range, head)
        .await;
    if download && matches!(outcome.status, 200 | 206) {
        // A row deleted since the bytes were opened still downloads, under
        // a plain name.
        let name = match state.library.track(&caller.id, &internal).await {
            Ok(Some(track)) => {
                download_filename(&track.title, track.file_format.as_deref().unwrap_or(""))
            }
            Ok(None) => download_filename("track", served_format(&outcome)),
            Err(failed) => return ApiError::from(failed).into_response(),
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

/// The format of the bytes an outcome serves, from its content type.
/// Empty when it names none.
fn served_format(outcome: &ByteOutcome) -> &'static str {
    let content_type = outcome
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map_or("", |(_, value)| value.as_str());
    droppedneedle_media::stream::routes::extension_for_content_type(content_type).unwrap_or("")
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

/// `HEAD /Audio/...`: the same status and headers GET would answer, the
/// transcode decision included (200/206/416; HEAD honors Range), always
/// with an empty body and no transcode started (v2 `_audio_stream_head`).
pub(super) async fn audio_head(
    State(state): State<JellyfinState>,
    Path((item_id, tail)): Path<(String, String)>,
    caller: Authed,
    request: Request<Body>,
) -> Response {
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
    let mut state = state;
    state.engine = state.engine.for_caller(&caller.principal.id);
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
    if tail == "universal" {
        universal(&state, &headers, query.as_deref(), &item_id, true).await
    } else {
        audio_stream(&state, &headers, query.as_deref(), &item_id, true).await
    }
}

/// `/universal` container negotiation (v2 `_universal`): a source format
/// the client lists in `Container` asks for nothing, else `AudioCodec` is
/// the requested format; `MaxStreamingBitrate` is the ceiling and
/// `StartTimeTicks` the offset.
pub(super) async fn universal(
    state: &JellyfinState,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
    head: bool,
) -> Response {
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let accepted = accepted_containers(q.get("Container"));
    let track_format = match state.library.track("", &internal).await {
        Ok(track) => track.and_then(|t| t.file_format),
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    let format = if track_format
        .as_deref()
        .is_some_and(|f| accepted.contains(&f.to_lowercase()))
    {
        None
    } else {
        map_codec(q.get("AudioCodec"))
    };
    let params = StreamParams {
        format,
        max_bitrate_kbps: ceiling_kbps(params::qint(&q, "MaxStreamingBitrate", 0)),
        start_seconds: params::qint(&q, "StartTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64,
        estimate_content_length: false,
    };
    play(state, &internal, &params, header(headers, "range"), head).await
}

/// `/stream[.ext]`: `static=true` (case-insensitive) asks for the original
/// (v2 `_audio_stream`); otherwise `audioCodec` is the requested format,
/// `audioBitRate` the ceiling and `startTimeTicks` the offset.
pub(super) async fn audio_stream(
    state: &JellyfinState,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
    head: bool,
) -> Response {
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let params = if q.get("static").unwrap_or("").eq_ignore_ascii_case("true") {
        StreamParams::original()
    } else {
        StreamParams {
            format: map_codec(q.get("audioCodec")),
            max_bitrate_kbps: ceiling_kbps(params::qint(&q, "audioBitRate", 0)),
            start_seconds: params::qint(&q, "startTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64,
            estimate_content_length: false,
        }
    };
    play(state, &internal, &params, header(headers, "range"), head).await
}

/// GET+POST PlaybackInfo (v2 `_playback_info`). `DirectStreamUrl` embeds
/// `?api_key=<token>` for headerless players; the transcoding address and
/// container appear only when the policy says transcode; Finamp's 15
/// non-null fields are always present (MediaStream 5 + MediaSourceInfo 10,
/// v2 issue #438).
pub(super) async fn playback_info(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Response {
    let raw_query = request.uri().query().map(str::to_owned);
    let is_post = request.method() == axum::http::Method::POST;
    let advertised = local_address(&state, &request);
    let body = request.into_body();
    let internal = match decode_track(&state, &item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let track = match state.library.track(&authed.principal.id, &internal).await {
        Ok(Some(track)) => track,
        Ok(None) => return error(StatusCode::NOT_FOUND),
        Err(failed) => return ApiError::from(failed).into_response(),
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
    let settings = state.settings.jellyfin();
    let ceiling = max_bps.and_then(ceiling_kbps);
    // The same decision the stream gateway makes, on the same catalog facts.
    let plan = decide(
        &TrackInfo {
            file_format: track.file_format.clone().unwrap_or_default(),
            bitrate_kbps: track.bitrate.map(i64::from),
            duration_seconds: track.duration_seconds.unwrap_or(0.0),
        },
        None,
        ceiling,
        false,
        0.0,
        &settings.transcode_policy(),
        settings.ffmpeg_available,
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
    let mut src = builders::media_source(
        &track,
        &item_id,
        settings.transcoding_enabled && settings.ffmpeg_available,
    );
    src.direct_stream_url = Some(direct_url);
    if let Some(out) = plan.out_format() {
        let codec = out.name();
        // Root-relative yet inside the deployment prefix: players resolve
        // against the advertised origin, so a bare path would escape the
        // base path under non-empty BASE_PATH deployments (v2 `_playback_info`).
        // The ceiling rides along: the client follows this address alone, and
        // /universal reads MaxStreamingBitrate as its ceiling.
        let cap = match max_bps {
            Some(bps) if ceiling.is_some() => format!("&MaxStreamingBitrate={bps}"),
            _ => String::new(),
        };
        src.transcoding_url = Some(format!(
            "{}/jellyfin/Audio/{item_id}/universal?AudioCodec={codec}&Container={codec}{cap}&PlaySessionId={psid}&api_key={}",
            state.listener.base_path, authed.principal.token,
        ));
        src.transcoding_container = Some(out.container().to_owned());
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
