//! Media handlers: cover art, stream, download, transcode, avatar.
//! v2: the Subsonic router's endpoint functions.

use super::error::{NOT_AUTHORIZED, NOT_FOUND, SubsonicError};
use super::ids::{IdKind, decode, decode_expect};
use super::library::current_id;
use super::models::{SStreamDetails, STranscodeDecision};
use super::picture::PictureKind;
use super::store::ClientInfo;
use super::stream::{
    AudioBody, MAX_OFFSET_SECONDS, ServeError, ServedAudio, StreamParams, cover_bucket,
    download_filename, placeholder_cover, serve,
};
use super::{AVATAR_FORBIDDEN_MESSAGE, Ctx, Outcome};
use crate::adapters::library::CompatError;
use crate::adapters::subsonic_store::SubsonicStore;
use droppedneedle_media::stream::leases::RETRY_AFTER_SECONDS;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

/// Audio as a binary outcome: bytes in memory, or a live stream.
fn audio_outcome(
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: AudioBody,
) -> Outcome {
    match body {
        AudioBody::Bytes(body) => Outcome::Binary {
            status,
            content_type,
            headers,
            body,
        },
        AudioBody::Live(body) => Outcome::Stream {
            status,
            content_type,
            headers,
            body,
        },
    }
}

/// Map a serve failure onto a binary outcome or a protocol error:
/// 416 carries `Content-Range: bytes */N` with no body; a full pool is
/// 429 + `Retry-After: 1`; anything else follows the binary-vs-envelope
/// split in dispatch.
pub fn serve_outcome(served: Result<ServedAudio, ServeError>) -> Result<Outcome, SubsonicError> {
    match served {
        Ok(audio) => Ok(audio_outcome(
            audio.status,
            audio.content_type,
            audio.headers,
            audio.body,
        )),
        Err(ServeError::RangeUnsatisfiable(size)) => Ok(Outcome::Binary {
            status: 416,
            content_type: "application/octet-stream".to_owned(),
            headers: vec![("Content-Range".to_owned(), format!("bytes */{size}"))],
            body: Vec::new(),
        }),
        Err(ServeError::CapacityFull) => Ok(Outcome::Binary {
            status: 429,
            content_type: "text/plain".to_owned(),
            headers: vec![("Retry-After".to_owned(), RETRY_AFTER_SECONDS.to_string())],
            body: Vec::new(),
        }),
        Err(ServeError::Subsonic(err)) => Err(err),
    }
}

/// The placeholder an art miss gets, coloured from `seed` (see
/// [`placeholder_seed`]). `None` only if it could not be drawn.
fn placeholder(seed: &str) -> Option<Outcome> {
    Some(Outcome::Binary {
        status: 200,
        content_type: "image/webp".to_owned(),
        headers: vec![(
            "Cache-Control".to_owned(),
            "public, max-age=86400".to_owned(),
        )],
        body: placeholder_cover(seed)?,
    })
}

/// What a placeholder is coloured from, as the web UI picks it: the
/// MusicBrainz id when the item has one, otherwise its name.
fn placeholder_seed<'a>(id: Option<&'a str>, name: &'a str) -> &'a str {
    id.filter(|id| is_valid_mbid(id)).unwrap_or(name)
}

/// Library art found, with the long-lived cache headers it has always had.
fn immutable(body: Vec<u8>, content_type: String) -> Outcome {
    Outcome::Binary {
        status: 200,
        content_type,
        headers: vec![(
            "Cache-Control".to_owned(),
            "public, max-age=31536000, immutable".to_owned(),
        )],
        body,
    }
}

/// An album's cover or an artist's picture in a size bucket, or the
/// placeholder, coloured from the id, when there is none. The caller has
/// checked that the album or artist is in the library. `None` only if the
/// placeholder could not be drawn. `getCoverArt` and the open picture
/// address both serve these through here.
pub async fn library_picture(
    store: &SubsonicStore,
    kind: PictureKind,
    internal: &str,
    bucket: &str,
) -> Result<Option<Outcome>, CompatError> {
    let art = match kind {
        PictureKind::Album => store.release_group_cover(internal, bucket).await?,
        PictureKind::Artist => {
            store
                .artist_image(internal, bucket.parse::<i64>().ok())
                .await?
        }
    };
    Ok(match art {
        Some((body, content_type)) => Some(immutable(body, content_type)),
        None => placeholder(internal),
    })
}

/// An album's or artist's picture for `getCoverArt`: the read that
/// resolves the id is the existence check, so no album or artist is
/// loaded. `missing` is the answer for an id nothing live holds.
async fn picture_of(
    ctx: &Ctx<'_>,
    kind: PictureKind,
    internal: &str,
    bucket: &str,
    missing: &str,
) -> Result<Outcome, SubsonicError> {
    let id_kind = match kind {
        PictureKind::Album => IdKind::Album,
        PictureKind::Artist => IdKind::Artist,
    };
    let live = ctx
        .store
        .current_id(id_kind, internal)
        .await
        .map_err(Ctx::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, missing))?;
    library_picture(ctx.store, kind, &live, bucket)
        .await
        .map_err(Ctx::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, missing))
}

/// Cover art: album/track -> release-group art, artist -> artist image,
/// playlist -> its uploaded cover (Feishin #287) or one made from its
/// tracks. Misses serve a WebP placeholder coloured from the item;
/// non-playlist hits carry immutable cache headers whatever cover version
/// the id carries; `size` buckets to 250/500/1200. An album or artist is
/// found by the one read that resolves its id (an alias or a merged id
/// names the live one). HEAD is served by the HTTP layer (which strips
/// bodies); only the playlist branch needs explicit headers.
pub async fn cover_art(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
    let sid = ctx.p("id")?.unwrap_or_default();
    // Unknown prefix -> 70 -> 404-as-text on the binary path. An album's
    // cover version is dropped here, so any version opens today's picture.
    let (kind, internal) = decode(&sid)?;
    let bucket = cover_bucket(ctx.pint("size", None, Some(1), Some(2000))?);
    match kind {
        IdKind::Album => {
            picture_of(
                ctx,
                PictureKind::Album,
                &internal,
                bucket,
                "Album not found",
            )
            .await
        }
        IdKind::Artist => {
            picture_of(
                ctx,
                PictureKind::Artist,
                &internal,
                bucket,
                "Artist not found",
            )
            .await
        }
        IdKind::Track => {
            let internal = current_id(ctx, kind, internal).await?;
            let track = ctx
                .store
                .get_track(&internal)
                .await
                .map_err(Ctx::store_err)?
                .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
            let mut art = None;
            if let Some(rg) = &track.rg_mbid {
                art = ctx
                    .store
                    .release_group_cover(rg, bucket)
                    .await
                    .map_err(Ctx::store_err)?;
            }
            match art {
                Some((body, content_type)) => Ok(immutable(body, content_type)),
                None => placeholder(placeholder_seed(
                    track.rg_mbid.as_deref(),
                    track.album_title.as_deref().unwrap_or(&track.title),
                ))
                .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found")),
            }
        }
        IdKind::Playlist => {
            let user_id = ctx.user()?.user_id().to_owned();
            let art = ctx
                .store
                .playlist_cover(&internal, &user_id)
                .await
                .map_err(Ctx::store_err)?;
            match art {
                // Playlists without a cover get the neutral pair, as on the web.
                None => placeholder("")
                    .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Playlist not found")),
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

/// Stream: the request's `format`, `maxBitRate`, `timeOffset` and
/// `estimateContentLength` go to the stream gateway, which decides direct
/// or transcode. `format=raw` always serves the original bytes;
/// `timeOffset` applies to transcodes only; `estimateContentLength` adds
/// Content-Length on transcodes only. A local miss falls through to the
/// plugin fallback in the gateway adapter, else 70 (v2 `_stream`).
pub async fn stream(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
    let fid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let fid = current_id(ctx, IdKind::Track, fid).await?;
    let params = StreamParams {
        format: ctx.params.one_of("format", &["raw", "mp3", "opus"], None)?,
        max_bitrate_kbps: ctx.pint("maxBitRate", None, Some(0), Some(1_000_000))?,
        start_seconds: ctx
            .pfloat("timeOffset", Some(0.0), Some(0.0), Some(MAX_OFFSET_SECONDS))?
            .unwrap_or(0.0),
        estimate_content_length: ctx.pbool("estimateContentLength", false)?,
    };
    serve_outcome(
        serve(
            ctx.audio,
            &fid,
            &params,
            ctx.range.as_deref(),
            ctx.method == "HEAD",
            None,
        )
        .await,
    )
}

/// Download: always original bytes, with a sanitized attachment filename.
/// A user the admin's download setting excludes is refused with code 50,
/// the Subsonic API's "not authorized for the given operation".
pub async fn download(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
    if !ctx.settings.download_access.admits(ctx.user()?.role()) {
        return Err(SubsonicError::new(
            NOT_AUTHORIZED,
            "Library downloads are restricted by the administrator",
        ));
    }
    let fid = decode_expect(&ctx.p("id")?.unwrap_or_default(), IdKind::Track)?;
    let fid = current_id(ctx, IdKind::Track, fid).await?;
    let track = ctx
        .store
        .get_track(&fid)
        .await
        .map_err(Ctx::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let filename = download_filename(&track.title, track.file_format.as_deref().unwrap_or(""));
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));
    serve_outcome(
        serve(
            ctx.audio,
            &fid,
            &StreamParams::original(),
            ctx.range.as_deref(),
            ctx.method == "HEAD",
            Some(disposition),
        )
        .await,
    )
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
pub async fn transcode_decision(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
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
    let file_id = current_id(ctx, IdKind::Track, file_id).await?;
    let track = ctx
        .store
        .get_track(&file_id)
        .await
        .map_err(Ctx::store_err)?
        .ok_or_else(|| SubsonicError::new(NOT_FOUND, "Song not found"))?;
    let user_id = ctx.user()?.user_id().to_owned();
    let client = ClientInfo {
        name: client.name,
        platform: client.platform,
        max_audio_bitrate: client.max_audio_bitrate,
        max_transcoding_audio_bitrate: client.max_transcoding_audio_bitrate,
        direct_play_profiles: client.direct_play_profiles,
        transcoding_profiles: client.transcoding_profiles,
        codec_profiles: client.codec_profiles,
    };
    super::advanced::validate(&client)?;
    let decision = ctx
        .store
        .advanced_decide(&track, &client, &user_id)
        .await
        .map_err(Ctx::store_err)?;
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
    Outcome::keyed(
        "transcodeDecision",
        STranscodeDecision {
            canDirectPlay: decision.can_direct_play,
            canTranscode: decision.can_transcode,
            transcodeReason: decision.transcode_reason,
            errorReason: decision.error_reason,
            transcodeParams: decision.transcode_params,
            sourceStream: details(decision.source_stream),
            transcodeStream: details(decision.transcode_stream),
        },
    )
}

/// Transcode stream: validates the signed parameters. Direct parameters
/// serve the original file; otherwise their format and bitrate go to the
/// stream gateway as the request, with the offset (estimate off).
pub async fn transcode_stream(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
    if ctx
        .params
        .one_of("mediaType", &["song", "podcast"], None)?
        .as_deref()
        != Some("song")
    {
        return Err(SubsonicError::new(10, "Only song transcoding is supported"));
    }
    let file_id = decode_expect(&ctx.p("mediaId")?.unwrap_or_default(), IdKind::Track)?;
    let file_id = current_id(ctx, IdKind::Track, file_id).await?;
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
    let user_id = ctx.user()?.user_id().to_owned();
    let (direct, out_format, bitrate) = ctx
        .store
        .decode_transcode_params(&params, &user_id, &file_id)
        .await
        .map_err(Ctx::store_err)?;
    let params = if direct {
        StreamParams::original()
    } else {
        if !ctx.settings.transcoding_enabled {
            return Err(SubsonicError::new(0, "Transcoding is disabled"));
        }
        let (Some(format), Some(bitrate)) = (out_format, bitrate) else {
            return Err(SubsonicError::new(10, "Invalid transcode parameters"));
        };
        StreamParams {
            format: Some(format),
            max_bitrate_kbps: Some(bitrate),
            start_seconds: offset,
            estimate_content_length: false,
        }
    };
    serve_outcome(
        serve(
            ctx.audio,
            &file_id,
            &params,
            ctx.range.as_deref(),
            ctx.method == "HEAD",
            None,
        )
        .await,
    )
}

/// Avatar: self-only. A username that is not the caller's is the one
/// direct 403-as-text (`_binary_error(50)`); a missing avatar is 70.
pub async fn avatar(ctx: &Ctx<'_>) -> Result<Outcome, SubsonicError> {
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
        .map_err(Ctx::store_err)?
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
