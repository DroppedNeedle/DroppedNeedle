//! Technical probe and decode through symphonia.
//!
//! Sample rate, channel count, and codec come from the decode side.
//! Duration is decode-counted (every delivered sample is tallied), except
//! for ADTS, where the stage-1 verdict prescribes the demux-count fallback:
//! packets x 1024 / rate, with no decode. Bitrate and bit depth ride along
//! from lofty's header parse, with v2's suppression rules (meaningful bit
//! depth only for lossless containers, and for M4A only when ALAC).
//!
//! Mid-stream decode wobbles are tolerated the way v2 tolerated fpcalc's
//! nonzero exit with output (F-044): the samples decoded so far stand, and
//! the partial flag tells downstream to corroborate before acting.

use std::fs::File;
use std::path::Path;

use lofty::file::AudioFile as _;
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_ALAC;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia_adapter_libopus::OpusDecoder;

use super::{AudioFormat, TagsError, format_for_path};

/// Technical properties of the audio stream and file on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInfo {
    pub duration_seconds: f64,
    /// Container-average kilobits per second (tags included).
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub file_format: String,
    pub file_size_bytes: u64,
    /// Meaningful bit depth, or `None` for lossy containers.
    pub bit_depth: Option<u8>,
}

/// Interleaved s16 PCM plus the stream layout it was decoded with.
pub(crate) struct DecodedPcm {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u32,
    /// True when a mid-stream error was tolerated (v2 F-044).
    pub partial: bool,
}

/// Codec registry with every enabled symphonia codec plus Opus via the
/// stage-1 adapter (`symphonia-adapter-libopus`).
fn codec_registry() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<OpusDecoder>();
    registry
}

/// Probe technical info. WMA is rejected before any byte is read.
pub fn probe(path: &Path) -> Result<AudioInfo, TagsError> {
    let format = format_for_path(path)?;
    let file_size_bytes = path.metadata().map(|meta| meta.len()).unwrap_or(0);
    if format == AudioFormat::Aac {
        return probe_adts(path, file_size_bytes);
    }

    let opened = open_reader(path, format)?;
    let is_alac = opened.params.codec == CODEC_ID_ALAC;
    let duration_seconds =
        decode_count_duration(opened.reader, opened.params.clone()).map_err(|reason| {
            TagsError::Probe {
                path: path.display().to_string(),
                reason,
            }
        })?;
    let bitrate = average_bitrate(file_size_bytes, duration_seconds);
    let bit_depth = bit_depth_for(path, format, is_alac);
    Ok(AudioInfo {
        duration_seconds,
        bitrate,
        sample_rate: opened.sample_rate,
        channels: opened.channels,
        file_format: format.as_str().to_owned(),
        file_size_bytes,
        bit_depth,
    })
}

struct OpenStream {
    reader: Box<dyn FormatReader>,
    track_id: u32,
    params: AudioCodecParameters,
    sample_rate: u32,
    channels: u32,
}

fn open_reader(path: &Path, format: AudioFormat) -> Result<OpenStream, TagsError> {
    let file = File::open(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(format.as_str());
    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| TagsError::Probe {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| TagsError::Probe {
            path: path.display().to_string(),
            reason: "no default audio track".to_owned(),
        })?;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(params)) => params.clone(),
        _ => {
            return Err(TagsError::Probe {
                path: path.display().to_string(),
                reason: "default track is not audio".to_owned(),
            });
        }
    };
    let sample_rate = params.sample_rate.ok_or_else(|| TagsError::Probe {
        path: path.display().to_string(),
        reason: "stream has no sample rate".to_owned(),
    })?;
    let channels = params
        .channels
        .as_ref()
        .map_or(0, |layout| layout.count() as u32);
    if channels == 0 {
        return Err(TagsError::Probe {
            path: path.display().to_string(),
            reason: "stream has no channels".to_owned(),
        });
    }
    let track_id = track.id;
    Ok(OpenStream {
        reader,
        track_id,
        params,
        sample_rate,
        channels,
    })
}

/// ADTS has no trustworthy container duration (stage-1 measured symphonia's
/// at 10x wrong), so duration comes from the packet count: every ADTS
/// frame carries exactly 1024 samples.
fn probe_adts(path: &Path, file_size_bytes: u64) -> Result<AudioInfo, TagsError> {
    let opened = open_reader(path, AudioFormat::Aac)?;
    let mut reader = opened.reader;
    let mut packets: u64 = 0;
    while let Ok(Some(packet)) = reader.next_packet() {
        if packet.track_id == opened.track_id {
            packets += 1;
        }
    }
    if packets == 0 {
        return Err(TagsError::Probe {
            path: path.display().to_string(),
            reason: "no ADTS packets found".to_owned(),
        });
    }
    let duration_seconds = packets as f64 * 1024.0 / f64::from(opened.sample_rate);
    Ok(AudioInfo {
        duration_seconds,
        bitrate: average_bitrate(file_size_bytes, duration_seconds),
        sample_rate: opened.sample_rate,
        channels: opened.channels,
        file_format: AudioFormat::Aac.as_str().to_owned(),
        file_size_bytes,
        bit_depth: None,
    })
}

/// Decode the whole default track and tally delivered samples. Slow for
/// long files, but exact everywhere container math is not.
fn decode_count_duration(
    mut reader: Box<dyn FormatReader>,
    params: AudioCodecParameters,
) -> Result<f64, String> {
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| "stream has no sample rate".to_owned())?;
    let track_id = reader
        .default_track(TrackType::Audio)
        .map(|track| track.id)
        .ok_or_else(|| "no default audio track".to_owned())?;
    let registry = codec_registry();
    let mut decoder = registry
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|error| error.to_string())?;
    let mut frames: u64 = 0;
    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::ResetRequired) => break,
            Err(error) => return Err(error.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                frames += decoded.frames() as u64;
            }
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::ResetRequired) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(frames as f64 / f64::from(sample_rate))
}

/// Decode to interleaved s16, stopping after `max_seconds` of audio when
/// set. Tolerates mid-stream errors with the partial flag (v2 F-044); a
/// decode that yields nothing at all is a hard error.
pub(crate) fn decode_pcm_s16(
    path: &Path,
    format: AudioFormat,
    max_seconds: Option<f64>,
) -> Result<DecodedPcm, TagsError> {
    let opened = open_reader(path, format).map_err(|error| match error {
        TagsError::Probe { reason, .. } => TagsError::Decode {
            path: path.display().to_string(),
            reason,
        },
        other => other,
    })?;
    let registry = codec_registry();
    let mut decoder = registry
        .make_audio_decoder(&opened.params, &AudioDecoderOptions::default())
        .map_err(|error| TagsError::Decode {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    let mut reader = opened.reader;
    let sample_cap =
        max_seconds.map(|seconds| (seconds * f64::from(opened.sample_rate)).ceil() as usize);
    let mut samples = Vec::new();
    let mut partial = false;
    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::ResetRequired) => break,
            Err(_) => {
                partial = true;
                break;
            }
        };
        if packet.track_id != opened.track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => append_interleaved(&mut samples, &decoded),
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::ResetRequired) => {
                partial = true;
                continue;
            }
            Err(_) => {
                partial = true;
                break;
            }
        }
        if sample_cap.is_some_and(|cap| samples.len() / opened.channels as usize >= cap) {
            break;
        }
    }
    if samples.is_empty() {
        return Err(TagsError::Decode {
            path: path.display().to_string(),
            reason: "decoder produced no samples".to_owned(),
        });
    }
    if let Some(cap) = sample_cap {
        samples.truncate(cap * opened.channels as usize);
    }
    Ok(DecodedPcm {
        samples,
        sample_rate: opened.sample_rate,
        channels: opened.channels,
        partial,
    })
}

fn append_interleaved(out: &mut Vec<i16>, decoded: &GenericAudioBufferRef) {
    let channels = decoded.spec().channels().count();
    let mut chunk = vec![0i16; decoded.frames() * channels];
    decoded.copy_to_slice_interleaved(&mut chunk);
    out.extend_from_slice(&chunk);
}

/// Container-average bitrate. Packet-byte tallying is not available:
/// symphonia 0.6 demuxers do not populate packet payload sizes uniformly
/// (the OGG reader yields one-byte packet shells), so the honest uniform
/// figure divides the whole file by the decode-counted duration.
fn average_bitrate(file_size_bytes: u64, duration_seconds: f64) -> u32 {
    if duration_seconds <= 0.0 {
        return 0;
    }
    (file_size_bytes as f64 * 8.0 / duration_seconds / 1000.0).round() as u32
}

/// Bit depth from lofty's header parse with v2's suppression rules: only
/// for lossless containers (FLAC/WAV), and for M4A only when ALAC.
fn bit_depth_for(path: &Path, format: AudioFormat, is_alac: bool) -> Option<u8> {
    let lossy = match format {
        AudioFormat::Flac | AudioFormat::Wav => false,
        AudioFormat::M4a => !is_alac,
        _ => true,
    };
    if lossy {
        return None;
    }
    lofty::read_from_path(path)
        .ok()
        .and_then(|tagged| tagged.properties().bit_depth())
        .filter(|depth| *depth > 0)
}
