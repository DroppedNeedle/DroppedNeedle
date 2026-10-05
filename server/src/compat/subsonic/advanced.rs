//! OpenSubsonic transcoding v1: the getTranscodeDecision policy and the
//! sealed parameters getTranscodeStream accepts (v2
//! `AdvancedTranscodeService`).
//!
//! The decision is pure: it compares one track with the client's
//! direct-play, transcoding and codec profiles. The parameters it hands
//! out are sealed with the config key and expire after five minutes; they
//! name the user and the file, so they cannot be replayed for another
//! track or account.

use serde::{Deserialize, Serialize};

use super::error::SubsonicError;
use super::media::client_info::{CodecLimitation, DirectPlayProfile};
use super::store::{ClientInfo, StreamDetailsData, TranscodeDecisionData};
use super::views::ViewTrack;
use crate::runtime_config::Crypto;

/// How long sealed parameters stay valid.
const TOKEN_TTL_SECONDS: i64 = 5 * 60;
/// Output formats the transcoder produces.
const OUTPUT_FORMATS: [&str; 2] = ["mp3", "opus"];
/// Protocols a profile may name.
const PROTOCOLS: [&str; 2] = ["http", "hls"];
/// Limitation names a codec profile may use.
const LIMITATION_NAMES: [&str; 5] = [
    "audioChannels",
    "audioBitrate",
    "audioProfile",
    "audioSamplerate",
    "audioBitdepth",
];
/// Comparisons a limitation may use.
const COMPARISONS: [&str; 4] = ["Equals", "NotEquals", "LessThanEqual", "GreaterThanEqual"];

/// Server-side switches the decision honors.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Transcoding allowed at all.
    pub transcoding_enabled: bool,
    /// ffmpeg present.
    pub ffmpeg_available: bool,
    /// Server quality ceiling, kbps.
    pub max_bitrate_kbps: i64,
}

/// What the sealed parameters let the stream route do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    user_id: String,
    file_id: String,
    direct: bool,
    out_format: Option<String>,
    bitrate_kbps: Option<i64>,
    expires_at: i64,
}

fn invalid(message: &str) -> SubsonicError {
    SubsonicError::new(10, message)
}

/// Reject malformed client descriptions with code 10 (v2 limits).
pub fn validate(client: &ClientInfo) -> Result<(), SubsonicError> {
    let bad_text = |value: &str| value.is_empty() || value.len() > 256;
    if bad_text(&client.name) || bad_text(&client.platform) {
        return Err(invalid("Invalid transcode client information"));
    }
    for value in [
        client.max_audio_bitrate,
        client.max_transcoding_audio_bitrate,
    ]
    .into_iter()
    .flatten()
    {
        // 0 means "no limit" (Feishin sends it).
        if value != 0 && !(1..=1_000_000_000).contains(&value) {
            return Err(invalid("Invalid transcode bitrate"));
        }
    }
    if client.direct_play_profiles.len() > 100 || client.transcoding_profiles.len() > 100 {
        return Err(invalid("Too many transcode profiles"));
    }
    let bad_channels = |value: Option<i64>| value.is_some_and(|n| !(1..=128).contains(&n));
    for profile in &client.direct_play_profiles {
        if profile
            .protocols
            .iter()
            .any(|protocol| !PROTOCOLS.contains(&protocol.as_str()))
            || bad_channels(profile.max_audio_channels)
        {
            return Err(invalid("Invalid direct-play profile"));
        }
    }
    for profile in &client.transcoding_profiles {
        if !PROTOCOLS.contains(&profile.protocol.as_str())
            || bad_channels(profile.max_audio_channels)
        {
            return Err(invalid("Invalid transcoding profile"));
        }
    }
    for profile in &client.codec_profiles {
        if profile.profile_type != "AudioCodec" || profile.limitations.len() > 100 {
            return Err(invalid("Invalid codec profile"));
        }
        for limitation in &profile.limitations {
            if !LIMITATION_NAMES.contains(&limitation.name.as_str())
                || !COMPARISONS.contains(&limitation.comparison.as_str())
                || limitation.values.is_empty()
                || limitation.values.len() > 100
            {
                return Err(invalid("Invalid codec limitation"));
            }
        }
    }
    Ok(())
}

fn channels(track: &ViewTrack) -> i64 {
    track.channels.unwrap_or(2)
}

fn direct_profile_matches(profile: &DirectPlayProfile, track: &ViewTrack, format: &str) -> bool {
    let listed = |values: &[String]| {
        values.is_empty() || values.iter().any(|value| value.to_lowercase() == format)
    };
    listed(&profile.containers)
        && listed(&profile.audio_codecs)
        && (profile.protocols.is_empty() || profile.protocols.iter().any(|p| p == "http"))
        && profile
            .max_audio_channels
            .is_none_or(|max| channels(track) <= max)
}

fn limitation_allows(limitation: &CodecLimitation, track: &ViewTrack) -> bool {
    let actual = match limitation.name.as_str() {
        "audioChannels" => track.channels,
        "audioBitrate" => track.bitrate.map(|kbps| kbps * 1000),
        "audioSamplerate" => track.sample_rate,
        "audioBitdepth" => track.bit_depth,
        _ => None,
    };
    let Some(actual) = actual else {
        return false;
    };
    let text = actual.to_string();
    match limitation.comparison.as_str() {
        "Equals" => limitation.values.contains(&text),
        "NotEquals" => !limitation.values.contains(&text),
        comparison => {
            let Some(expected) = limitation
                .values
                .first()
                .and_then(|value| value.parse::<f64>().ok())
            else {
                return false;
            };
            let actual = actual as f64;
            if comparison == "LessThanEqual" {
                actual <= expected
            } else {
                actual >= expected
            }
        }
    }
}

fn codec_profiles_allow(client: &ClientInfo, track: &ViewTrack, format: &str) -> bool {
    client
        .codec_profiles
        .iter()
        .filter(|profile| profile.name.to_lowercase() == format)
        .flat_map(|profile| profile.limitations.iter())
        .filter(|limitation| limitation.required)
        .all(|limitation| limitation_allows(limitation, track))
}

/// What the decision allows, before sealing.
#[derive(Debug, Clone)]
pub struct Decision {
    /// The wire answer, without parameters.
    pub answer: TranscodeDecisionData,
    /// `(direct, out format, bitrate kbps)` to seal, when anything plays.
    pub grant: Option<(bool, Option<String>, Option<i64>)>,
}

/// Decide how one track plays for one client (v2 `decide`).
pub fn decide(track: &ViewTrack, client: &ClientInfo, policy: Policy) -> Decision {
    let format = track.file_format.clone().unwrap_or_default().to_lowercase();
    let mut direct = client
        .direct_play_profiles
        .iter()
        .any(|profile| direct_profile_matches(profile, track, &format))
        && codec_profiles_allow(client, track, &format);
    if direct
        && let (Some(max), Some(kbps)) = (client.max_audio_bitrate, track.bitrate)
        && max > 0
        && kbps * 1000 > max
    {
        direct = false;
    }
    let profile = client.transcoding_profiles.iter().find(|profile| {
        profile.protocol == "http"
            && OUTPUT_FORMATS.contains(&profile.audio_codec.to_lowercase().as_str())
            && OUTPUT_FORMATS.contains(&profile.container.to_lowercase().as_str())
            && profile
                .max_audio_channels
                .is_none_or(|max| channels(track) <= max)
    });
    let can_transcode = profile.is_some() && policy.transcoding_enabled && policy.ffmpeg_available;
    let unknown = || "unknown".to_owned();
    let source = StreamDetailsData {
        protocol: "http".to_owned(),
        container: if format.is_empty() {
            unknown()
        } else {
            format.clone()
        },
        codec: if format.is_empty() {
            unknown()
        } else {
            format.clone()
        },
        audio_channels: track.channels,
        audio_bitrate: track.bitrate.map(|kbps| kbps * 1000),
        audio_samplerate: track.sample_rate,
        audio_bitdepth: track.bit_depth,
    };
    if direct {
        return Decision {
            answer: TranscodeDecisionData {
                can_direct_play: true,
                can_transcode: false,
                source_stream: Some(source),
                ..TranscodeDecisionData::default()
            },
            grant: Some((true, None, None)),
        };
    }
    let Some(profile) = profile.filter(|_| can_transcode) else {
        return Decision {
            answer: TranscodeDecisionData {
                can_direct_play: false,
                can_transcode: false,
                transcode_reason: vec!["No supported transcoding profile".to_owned()],
                error_reason: Some("No compatible playback path".to_owned()),
                source_stream: Some(source),
                ..TranscodeDecisionData::default()
            },
            grant: None,
        };
    };
    let to_kbps = |bps: i64| (bps + 500) / 1000;
    let mut ceilings = vec![policy.max_bitrate_kbps];
    for limit in [
        client.max_audio_bitrate,
        client.max_transcoding_audio_bitrate,
    ]
    .into_iter()
    .flatten()
    .filter(|limit| *limit > 0)
    {
        ceilings.push(to_kbps(limit));
    }
    let bitrate = ceilings.into_iter().min().unwrap_or(320).max(64);
    let output = profile.audio_codec.to_lowercase();
    Decision {
        answer: TranscodeDecisionData {
            can_direct_play: false,
            can_transcode: true,
            transcode_reason: vec!["Source is outside direct-play profiles".to_owned()],
            source_stream: Some(source),
            transcode_stream: Some(StreamDetailsData {
                protocol: "http".to_owned(),
                container: profile.container.to_lowercase(),
                codec: output.clone(),
                audio_channels: Some(channels(track).min(profile.max_audio_channels.unwrap_or(2))),
                audio_bitrate: Some(bitrate * 1000),
                audio_samplerate: None,
                audio_bitdepth: None,
            }),
            ..TranscodeDecisionData::default()
        },
        grant: Some((false, Some(output), Some(bitrate))),
    }
}

/// Seal the grant for one user and file. `None` when the key refuses,
/// which the caller logs: the decision then ships without parameters.
pub fn seal(
    crypto: &Crypto,
    user_id: &str,
    file_id: &str,
    grant: (bool, Option<String>, Option<i64>),
    now_unix: i64,
) -> Option<String> {
    let token = Token {
        user_id: user_id.to_owned(),
        file_id: file_id.to_owned(),
        direct: grant.0,
        out_format: grant.1,
        bitrate_kbps: grant.2,
        expires_at: now_unix + TOKEN_TTL_SECONDS,
    };
    let json = serde_json::to_string(&token).ok()?;
    match crypto.encrypt(&json) {
        Ok(sealed) => Some(sealed),
        Err(error) => {
            tracing::error!(?error, "transcode parameters could not be sealed");
            None
        }
    }
}

/// Open sealed parameters for one user and file. Anything forged, expired,
/// meant for someone or something else, or inconsistent is `None`.
pub fn open(
    crypto: &Crypto,
    sealed: &str,
    user_id: &str,
    file_id: &str,
    now_unix: i64,
) -> Option<(bool, Option<String>, Option<i64>)> {
    let json = crypto.decrypt(sealed).ok()?;
    let token: Token = serde_json::from_str(&json).ok()?;
    let consistent = if token.direct {
        token.out_format.is_none() && token.bitrate_kbps.is_none()
    } else {
        token
            .out_format
            .as_deref()
            .is_some_and(|format| OUTPUT_FORMATS.contains(&format))
            && token
                .bitrate_kbps
                .is_some_and(|kbps| (64..=1_000_000).contains(&kbps))
    };
    (token.user_id == user_id
        && token.file_id == file_id
        && token.expires_at >= now_unix
        && consistent)
        .then_some((token.direct, token.out_format, token.bitrate_kbps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::subsonic::media::client_info::TranscodingProfile;

    fn flac() -> ViewTrack {
        ViewTrack {
            file_id: "t1".to_owned(),
            file_format: Some("flac".to_owned()),
            bitrate: Some(900),
            channels: Some(2),
            ..ViewTrack::default()
        }
    }

    fn mp3_only() -> ClientInfo {
        ClientInfo {
            name: "Player".to_owned(),
            platform: "android".to_owned(),
            direct_play_profiles: vec![DirectPlayProfile {
                containers: vec!["mp3".to_owned()],
                audio_codecs: Vec::new(),
                protocols: vec!["http".to_owned()],
                max_audio_channels: None,
            }],
            transcoding_profiles: vec![TranscodingProfile {
                container: "mp3".to_owned(),
                audio_codec: "mp3".to_owned(),
                protocol: "http".to_owned(),
                max_audio_channels: None,
            }],
            max_transcoding_audio_bitrate: Some(192_000),
            ..ClientInfo::default()
        }
    }

    #[test]
    fn unplayable_source_transcodes_under_the_lowest_ceiling() {
        let policy = Policy {
            transcoding_enabled: true,
            ffmpeg_available: true,
            max_bitrate_kbps: 320,
        };
        let decision = decide(&flac(), &mp3_only(), policy);
        assert!(decision.answer.can_transcode && !decision.answer.can_direct_play);
        assert_eq!(
            decision.grant,
            Some((false, Some("mp3".to_owned()), Some(192)))
        );
    }

    #[test]
    fn sealed_parameters_bind_user_file_and_time() {
        let crypto = Crypto::from_key_bytes(&[3u8; 32]).unwrap();
        let grant = (false, Some("mp3".to_owned()), Some(192));
        let sealed = seal(&crypto, "u1", "t1", grant.clone(), 1_000).unwrap();
        assert_eq!(open(&crypto, &sealed, "u1", "t1", 1_100), Some(grant));
        assert_eq!(
            open(&crypto, &sealed, "u2", "t1", 1_100),
            None,
            "other user"
        );
        assert_eq!(
            open(&crypto, &sealed, "u1", "t2", 1_100),
            None,
            "other file"
        );
        assert_eq!(open(&crypto, &sealed, "u1", "t1", 2_000), None, "expired");
        assert_eq!(open(&crypto, "v3:forged", "u1", "t1", 1_100), None);
    }
}
