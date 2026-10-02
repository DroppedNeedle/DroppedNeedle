//! Media handlers: cover art, stream, download, transcode, avatar.
//! v2: `backend/api/compat/subsonic/router.py` (endpoint functions).

use super::auth::Principal;
use super::error::{NOT_FOUND, SubsonicError};
use super::ids::{IdKind, decode, decode_expect};
use super::models::{Render, SStreamDetails, STranscodeDecision};
use super::store::{ClientInfo, Store};
use super::stream::{
    AudioBackend, MAX_OFFSET_SECONDS, PLACEHOLDER_SVG, ServeError, ServedAudio, StreamPlan,
    cover_bucket, decide, download_filename, estimate_transcode_length, serve_original,
    transcode_hint,
};
use super::{AVATAR_FORBIDDEN_MESSAGE, Ctx, Outcome};

/// Map a serve failure onto a binary outcome or a protocol error:
/// 416 carries `Content-Range: bytes */N` with no body; a full pool is
/// 429 + `Retry-After: 1`; anything else follows the binary-vs-envelope
/// split in dispatch.
pub fn serve_outcome(served: Result<ServedAudio, ServeError>) -> Result<Outcome, SubsonicError> {
    match served {
        Ok(audio) => Ok(Outcome::Binary {
            status: audio.status,
            content_type: audio.content_type,
            headers: audio.headers,
            body: audio.body,
        }),
        Err(ServeError::RangeUnsatisfiable(size)) => Ok(Outcome::Binary {
            status: 416,
            content_type: "application/octet-stream".to_owned(),
            headers: vec![("Content-Range".to_owned(), format!("bytes */{size}"))],
            body: Vec::new(),
        }),
        Err(ServeError::CapacityFull) => Ok(Outcome::Binary {
            status: 429,
            content_type: "text/plain".to_owned(),
            headers: vec![("Retry-After".to_owned(), "1".to_owned())],
            body: Vec::new(),
        }),
        Err(ServeError::Subsonic(err)) => Err(err),
    }
}

/// Cover art: album/track -> release-group art, artist -> artist image,
/// playlist -> playlist file (Feishin #287). Misses serve the gray SVG
/// placeholder; non-playlist hits carry immutable cache headers; `size`
/// buckets to 250/500/1200. HEAD is served by the HTTP layer (which
/// strips bodies); only the playlist branch needs explicit headers.
pub async fn cover_art<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let sid = ctx.p("id")?.unwrap_or_default();
    // Unknown prefix -> 70 -> 404-as-text on the binary path.
    let (kind, internal) = decode(&sid)?;
    let bucket = cover_bucket(ctx.pint("size", None, Some(1), Some(2000))?);
    let placeholder = || Outcome::Binary {
        status: 200,
        content_type: "image/svg+xml".to_owned(),
        headers: vec![(
            "Cache-Control".to_owned(),
            "public, max-age=86400".to_owned(),
        )],
        body: PLACEHOLDER_SVG.as_bytes().to_vec(),
    };
    let immutable = |body: Vec<u8>, content_type: String| Outcome::Binary {
        status: 200,
        content_type,
        headers: vec![(
            "Cache-Control".to_owned(),
            "public, max-age=31536000, immutable".to_owned(),
        )],
        body,
    };
    match kind {
        IdKind::Album => {
            if ctx
                .store
                .get_album(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .is_none()
            {
                return Err(SubsonicError::new(NOT_FOUND, "Album not found"));
            }
            let art = ctx
                .store
                .release_group_cover(&internal, bucket)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
            Ok(art
                .map(|(body, content_type)| immutable(body, content_type))
                .unwrap_or_else(placeholder))
        }
        IdKind::Track => {
            let track = ctx
                .store
                .get_track(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
            let mut art = None;
            if let Some(rg) = track.rg_mbid {
                art = ctx
                    .store
                    .release_group_cover(&rg, bucket)
                    .await
                    .map_err(Ctx::<P, S, B>::store_err)?;
            }
            Ok(art
                .map(|(body, content_type)| immutable(body, content_type))
                .unwrap_or_else(placeholder))
        }
        IdKind::Artist => {
            if ctx
                .store
                .get_artist_with_albums(&internal)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?
                .is_none()
            {
                return Err(SubsonicError::new(NOT_FOUND, "Artist not found"));
            }
            let px = bucket.parse::<i64>().ok();
            let art = ctx
                .store
                .artist_image(&internal, px)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
            Ok(art
                .map(|(body, content_type)| immutable(body, content_type))
                .unwrap_or_else(placeholder))
        }
        IdKind::Playlist => {
            let user_id = ctx.user()?.user_id().to_owned();
            let art = ctx
                .store
                .playlist_cover(&internal, &user_id)
                .await
                .map_err(Ctx::<P, S, B>::store_err)?;
            match art {
                None => Ok(placeholder()),
                Some((body, content_type)) => {
                    let headers = vec![
                        ("Content-Length".to_owned(), body.len().to_string()),
                        (
                            "Cache-Control".to_owned(),
                            "private, max-age=3600".to_owned(),
                        ),
                    ];
                    if ctx.method == "HEAD" {
                        return Ok(Outcome::Binary {
                            status: 200,
                            content_type,
                            headers,
                            body: Vec::new(),
                        });
                    }
                    Ok(Outcome::Binary {
                        status: 200,
                        content_type,
                        headers,
                        body,
                    })
                }
            }
        }
        _ => Err(SubsonicError::new(NOT_FOUND, "Cover art target not found")),
    }
}

/// Stream: `format=raw` always serves original bytes; otherwise the
/// `decide()` policy picks direct vs transcode. `timeOffset` applies to
/// transcodes only; `maxBitRate` never affects direct serves;
/// `estimateContentLength` adds Content-Length on transcodes only.
pub async fn stream<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let fid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let format = ctx.params.one_of("format", &["raw", "mp3", "opus"], None)?;
    let max_bitrate = ctx.pint("maxBitRate", None, Some(0), Some(1_000_000))?;
    let time_offset = ctx
        .pfloat("timeOffset", Some(0.0), Some(0.0), Some(MAX_OFFSET_SECONDS))?
        .unwrap_or(0.0);
    let estimate = ctx.pbool("estimateContentLength", false)?;
    if format.as_deref() == Some("raw") {
        // Local miss falls through to the plugin fallback behind the
        // backend seam, else 70 (v2 `_stream`).
        return serve_outcome(
            serve_original(
                ctx.audio,
                &fid,
                ctx.range.as_deref(),
                ctx.method == "HEAD",
                None,
            )
            .await,
        );
    }
    let track = ctx
        .store
        .get_track(&fid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let settings = ctx.settings;
    let plan = decide(
        track.file_format.as_deref().unwrap_or(""),
        track.bitrate,
        format.as_deref(),
        max_bitrate,
        false,
        time_offset,
        settings.transcoding_enabled,
        settings.ffmpeg_available,
        &settings.transcode_default_format,
        settings.transcode_max_bitrate_kbps,
    );
    if !plan.transcode {
        return serve_outcome(
            serve_original(
                ctx.audio,
                &fid,
                ctx.range.as_deref(),
                ctx.method == "HEAD",
                None,
            )
            .await,
        );
    }
    serve_transcode(ctx, &fid, &plan, track.duration_seconds, estimate).await
}

/// Download: always original bytes, with a sanitized attachment filename.
pub async fn download<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let fid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let track = ctx
        .store
        .get_track(&fid)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let filename = download_filename(&track.title, track.file_format.as_deref().unwrap_or(""));
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));
    serve_outcome(
        serve_original(
            ctx.audio,
            &fid,
            ctx.range.as_deref(),
            ctx.method == "HEAD",
            Some(disposition),
        )
        .await,
    )
}

/// Serve a transcode pipe: 200, `Accept-Ranges: none`,
/// `Cache-Control: no-store`, identity encoding; Content-Length only
/// with `estimateContentLength=true`. HEAD short-circuits before the
/// pipe (headers only, no lease, no ffmpeg): the content type derives
/// from the plan's output format instead of the backend.
pub async fn serve_transcode<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
    file_id: &str,
    plan: &StreamPlan,
    duration_seconds: f64,
    estimate: bool,
) -> Result<Outcome, SubsonicError> {
    if ctx.method == "HEAD" {
        let (content_type, _) = transcode_hint(plan.out_format.as_deref().unwrap_or(""));
        let mut headers = vec![
            ("Accept-Ranges".to_owned(), "none".to_owned()),
            ("Cache-Control".to_owned(), "no-store".to_owned()),
            ("Content-Encoding".to_owned(), "identity".to_owned()),
        ];
        if estimate && let Some(bitrate) = plan.out_bitrate_kbps {
            headers.push((
                "Content-Length".to_owned(),
                estimate_transcode_length(bitrate, duration_seconds, plan.start_seconds)
                    .to_string(),
            ));
        }
        return Ok(Outcome::Binary {
            status: 200,
            content_type: content_type.to_owned(),
            headers,
            body: Vec::new(),
        });
    }
    let (body, content_type) = match ctx.audio.transcode(file_id, plan).await {
        Ok(ok) => ok,
        Err(err) if err.is_full() => {
            return Ok(Outcome::Binary {
                status: 429,
                content_type: "text/plain".to_owned(),
                headers: vec![("Retry-After".to_owned(), "1".to_owned())],
                body: Vec::new(),
            });
        }
        Err(err) => return Err(SubsonicError::new(0, err.to_string())),
    };
    let mut headers = vec![
        ("Accept-Ranges".to_owned(), "none".to_owned()),
        ("Cache-Control".to_owned(), "no-store".to_owned()),
        ("Content-Encoding".to_owned(), "identity".to_owned()),
    ];
    if estimate && let Some(bitrate) = plan.out_bitrate_kbps {
        headers.push((
            "Content-Length".to_owned(),
            estimate_transcode_length(bitrate, duration_seconds, plan.start_seconds).to_string(),
        ));
    }
    Ok(Outcome::Binary {
        status: 200,
        content_type,
        headers,
        body,
    })
}

/// Transcode client description, strict: unknown fields rejected at
/// every level (v2 `forbid_unknown_fields`), body capped at 64KB.
pub mod client_info {
    use serde::Deserialize;

    /// Direct-play profile.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct DirectPlayProfile {
        /// Containers.
        #[serde(default)]
        pub containers: Vec<String>,
        /// Audio codecs.
        #[serde(rename = "audioCodecs", default)]
        pub audio_codecs: Vec<String>,
        /// Protocols.
        #[serde(default)]
        pub protocols: Vec<String>,
        /// Max channels.
        #[serde(rename = "maxAudioChannels", default)]
        pub max_audio_channels: Option<i64>,
    }

    /// Transcoding profile.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct TranscodingProfile {
        /// Container.
        #[serde(default)]
        pub container: String,
        /// Audio codec.
        #[serde(rename = "audioCodec", default)]
        pub audio_codec: String,
        /// Protocol.
        #[serde(default)]
        pub protocol: String,
        /// Max channels.
        #[serde(rename = "maxAudioChannels", default)]
        pub max_audio_channels: Option<i64>,
    }

    /// Codec limitation.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CodecLimitation {
        /// Name.
        #[serde(default)]
        pub name: String,
        /// Comparison.
        #[serde(default)]
        pub comparison: String,
        /// Values.
        #[serde(default)]
        pub values: Vec<String>,
        /// Required flag.
        #[serde(default)]
        pub required: bool,
    }

    /// Codec profile.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CodecProfile {
        /// Type.
        #[serde(rename = "type", default)]
        pub profile_type: String,
        /// Name.
        #[serde(default)]
        pub name: String,
        /// Limitations.
        #[serde(default)]
        pub limitations: Vec<CodecLimitation>,
    }

    /// Client description body.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ClientInfoJson {
        /// Client name.
        pub name: String,
        /// Client platform.
        pub platform: String,
        /// Max audio bitrate (0 = unset, Feishin #464/#468).
        #[serde(rename = "maxAudioBitrate", default)]
        pub max_audio_bitrate: Option<i64>,
        /// Max transcoding audio bitrate (0 = unset).
        #[serde(rename = "maxTranscodingAudioBitrate", default)]
        pub max_transcoding_audio_bitrate: Option<i64>,
        /// Direct-play profiles.
        #[serde(rename = "directPlayProfiles", default)]
        pub direct_play_profiles: Vec<DirectPlayProfile>,
        /// Transcoding profiles.
        #[serde(rename = "transcodingProfiles", default)]
        pub transcoding_profiles: Vec<TranscodingProfile>,
        /// Codec profiles.
        #[serde(rename = "codecProfiles", default)]
        pub codec_profiles: Vec<CodecProfile>,
    }
}

/// Parse the getTranscodeDecision JSON body (POST-only, JSON-only,
/// <= 64KB, unknown fields rejected).
pub fn parse_client_info(
    method: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<client_info::ClientInfoJson, SubsonicError> {
    if method != "POST" {
        return Err(SubsonicError::new(10, "getTranscodeDecision requires POST"));
    }
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if mime != "application/json" {
        return Err(SubsonicError::new(10, "getTranscodeDecision requires JSON"));
    }
    if body.is_empty() || body.len() > 64 * 1024 {
        return Err(SubsonicError::new(
            10,
            "Invalid transcode client information",
        ));
    }
    serde_json::from_slice(body)
        .map_err(|_| SubsonicError::new(10, "Invalid transcode client information"))
}

/// Transcode decision: POST-only, JSON-only client description,
/// mediaType=song only, signed params minted by the store.
pub async fn transcode_decision<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let client = parse_client_info(&ctx.method, ctx.content_type.as_deref(), &ctx.body)?;
    if ctx
        .params
        .one_of("mediaType", &["song", "podcast"], None)?
        .as_deref()
        != Some("song")
    {
        return Err(SubsonicError::new(10, "Only song transcoding is supported"));
    }
    let file_id = decode_expect(&ctx.p("mediaId")?.unwrap_or_default(), IdKind::Track)?;
    let track = ctx
        .store
        .get_track(&file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let user_id = ctx.user()?.user_id().to_owned();
    let decision = ctx
        .store
        .advanced_decide(
            &track,
            &ClientInfo {
                name: client.name,
                platform: client.platform,
                max_audio_bitrate: client.max_audio_bitrate,
                max_transcoding_audio_bitrate: client.max_transcoding_audio_bitrate,
            },
            &user_id,
        )
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    let details = |stream: Option<super::store::StreamDetailsData>| {
        stream.map(|s| SStreamDetails {
            protocol: s.protocol,
            container: s.container,
            codec: s.codec,
            audioChannels: s.audio_channels,
            audioBitrate: s.audio_bitrate,
            audioProfile: None,
            audioSamplerate: s.audio_samplerate,
            audioBitdepth: s.audio_bitdepth,
        })
    };
    Ok(Outcome::keyed(
        "transcodeDecision",
        STranscodeDecision {
            canDirectPlay: decision.can_direct_play,
            canTranscode: decision.can_transcode,
            transcodeReason: decision.transcode_reason,
            errorReason: decision.error_reason,
            transcodeParams: decision.transcode_params,
            sourceStream: details(decision.source_stream),
            transcodeStream: details(decision.transcode_stream),
        }
        .render(),
    ))
}

/// Transcode stream: validates signed params; direct params serve the
/// original file, else the ffmpeg pipe at the offset (estimate off).
pub async fn transcode_stream<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    if ctx
        .params
        .one_of("mediaType", &["song", "podcast"], None)?
        .as_deref()
        != Some("song")
    {
        return Err(SubsonicError::new(10, "Only song transcoding is supported"));
    }
    let file_id = decode_expect(&ctx.p("mediaId")?.unwrap_or_default(), IdKind::Track)?;
    let params = ctx
        .params
        .string_max("transcodeParams", None, 8192)?
        .unwrap_or_default();
    if params.is_empty() {
        return Err(SubsonicError::missing("transcodeParams"));
    }
    let offset = ctx
        .pint("offset", Some(0), Some(0), Some(604_800))?
        .unwrap_or(0)
        .max(0) as f64;
    let track = ctx
        .store
        .get_track(&file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let user_id = ctx.user()?.user_id().to_owned();
    let (direct, out_format, bitrate) = ctx
        .store
        .decode_transcode_params(&params, &user_id, &file_id)
        .await
        .map_err(Ctx::<P, S, B>::store_err)?;
    if direct {
        return serve_outcome(
            serve_original(
                ctx.audio,
                &file_id,
                ctx.range.as_deref(),
                ctx.method == "HEAD",
                None,
            )
            .await,
        );
    }
    if !ctx.settings.transcoding_enabled {
        return Err(SubsonicError::new(0, "Transcoding is disabled"));
    }
    match (out_format, bitrate) {
        (Some(out_format), Some(bitrate)) => {
            serve_transcode(
                ctx,
                &file_id,
                &StreamPlan {
                    transcode: true,
                    out_format: Some(out_format),
                    out_bitrate_kbps: Some(bitrate),
                    start_seconds: offset,
                },
                track.duration_seconds,
                false,
            )
            .await
        }
        _ => Err(SubsonicError::new(10, "Invalid transcode parameters")),
    }
}

/// Avatar: self-only. A username that is not the caller's is the ONE
/// direct 403-as-text (`_binary_error(50)`); a missing avatar is 70.
pub async fn avatar<P: Principal, S: Store, B: AudioBackend>(
    ctx: &'_ Ctx<'_, P, S, B>,
) -> Result<Outcome, SubsonicError> {
    let username = ctx.p("username")?.unwrap_or_default();
    let user = ctx.user()?;
    let folded = username.to_lowercase();
    let own = [
        user.username().to_lowercase(),
        user.username_display().to_lowercase(),
        user.display_name().to_lowercase(),
    ];
    if username.is_empty() || !own.contains(&folded) {
        return Ok(Outcome::Binary {
            status: 403,
            content_type: "text/plain".to_owned(),
            headers: Vec::new(),
            body: AVATAR_FORBIDDEN_MESSAGE.as_bytes().to_vec(),
        });
    }
    let resolved = ctx
        .store
        .resolve_avatar(user.user_id())
        .await
        .map_err(Ctx::<P, S, B>::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Avatar not found"))?;
    let (body, content_type) = resolved;
    let headers = vec![
        ("Content-Length".to_owned(), body.len().to_string()),
        (
            "Cache-Control".to_owned(),
            "private, max-age=3600".to_owned(),
        ),
    ];
    if ctx.method == "HEAD" {
        return Ok(Outcome::Binary {
            status: 200,
            content_type,
            headers,
            body: Vec::new(),
        });
    }
    Ok(Outcome::Binary {
        status: 200,
        content_type,
        headers,
        body,
    })
}
