//! AcoustID-style fingerprints through rusty-chromaprint.
//!
//! The stage-1 pipeline, wired as prescribed: symphonia decode, f64
//! downmix, first-120-seconds window, rubato SincFixedIn (sinc 256,
//! cutoff 0.95, cubic, Blackman-Harris, oversampling 256) to 11025 Hz
//! mono, truncate to the 120 s window, test2 preset, compress, base64
//! with the URL-safe unpadded alphabet (what fpcalc emits and what
//! AcoustID expects).
//!
//! Generation never touches the network; the AcoustID lookup itself is a
//! later slice's job. A truncated duration of zero is carried, not
//! rejected: v2 refused to submit those (an empty result set would look
//! like a genuine no-match), and the lookup side must keep that guard.

use std::path::Path;

use base64::Engine as _;
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use super::{TagsError, format_for_path};

/// Chromaprint's native rate: everything is resampled to this.
const TARGET_RATE: u32 = 11025;
/// fpcalc's `-length 120` window, in seconds and in target samples.
const WINDOW_SECONDS: f64 = 120.0;
const WINDOW_SAMPLES: usize = 120 * TARGET_RATE as usize;
/// Resampler input chunk, in frames.
const CHUNK_FRAMES: usize = 4096;

/// A generated fingerprint, ready to submit with its duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Compressed test2 print, URL-safe base64 without padding.
    pub fingerprint: String,
    /// Truncated total seconds, matching fpcalc's `DURATION`.
    pub duration_seconds: u32,
    /// True when the decode wobbled mid-stream but still yielded audio.
    pub partial_decode: bool,
    /// Raw u32 items before compression (spike-report parity info).
    pub raw_items: usize,
}

/// Generate the fingerprint for one file. WMA is rejected outright.
pub fn generate_fingerprint(path: &Path) -> Result<Fingerprint, TagsError> {
    let format = format_for_path(path)?;
    let info = super::probe(path).map_err(|error| match error {
        TagsError::Probe { reason, .. } => TagsError::Fingerprint {
            path: path.display().to_string(),
            reason,
        },
        other => other,
    })?;
    let pcm =
        super::probe::decode_pcm_s16(path, format, Some(WINDOW_SECONDS)).map_err(|error| {
            match error {
                TagsError::Decode { reason, .. } => TagsError::Fingerprint {
                    path: path.display().to_string(),
                    reason,
                },
                other => other,
            }
        })?;
    let mono = downmix_to_mono(&pcm.samples, pcm.channels);
    let at_target = resample_to_target(&mono, pcm.sample_rate, path)?;
    let quantized = quantize(&at_target);
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer
        .start(TARGET_RATE, 1)
        .map_err(|error| TagsError::Fingerprint {
            path: path.display().to_string(),
            reason: format!("chromaprint start failed: {error:?}"),
        })?;
    printer.consume(&quantized);
    printer.finish();
    let raw = printer.fingerprint().to_vec();
    let compressed = FingerprintCompressor::from(&config).compress(&raw);
    let fingerprint = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(compressed);
    Ok(Fingerprint {
        fingerprint,
        duration_seconds: info.duration_seconds.max(0.0) as u32,
        partial_decode: pcm.partial,
        raw_items: raw.len(),
    })
}

/// Channel mean in f64, the spike's downmix.
fn downmix_to_mono(samples: &[i16], channels: u32) -> Vec<f64> {
    let channels = channels.max(1) as usize;
    samples
        .chunks(channels)
        .map(|frame| frame.iter().map(|sample| f64::from(*sample)).sum::<f64>() / channels as f64)
        .collect()
}

/// Resample mono audio to 11025 Hz with the spike's cubic-256 setup, then
/// truncate to the 120 s window. Short inputs keep their (over-emitted)
/// tail, exactly like the spike's reference pipe.
fn resample_to_target(mono: &[f64], sample_rate: u32, path: &Path) -> Result<Vec<f64>, TagsError> {
    let fail = |reason: String| TagsError::Fingerprint {
        path: path.display().to_string(),
        reason,
    };
    if sample_rate == 0 || mono.is_empty() {
        return Err(fail("nothing to resample".to_owned()));
    }
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris,
    };
    let ratio = f64::from(TARGET_RATE) / f64::from(sample_rate);
    let mut resampler = SincFixedIn::<f64>::new(ratio, 1.0, params, CHUNK_FRAMES, 1)
        .map_err(|error| fail(format!("resampler setup failed: {error}")))?;
    let mut out: Vec<f64> = Vec::new();
    let mut chunks = mono.chunks(CHUNK_FRAMES).peekable();
    while let Some(chunk) = chunks.next() {
        let wave_in = [chunk];
        let mut wave_out = resampler.output_buffer_allocate(true);
        if chunks.peek().is_none() && chunk.len() < CHUNK_FRAMES {
            let (_, produced) = resampler
                .process_partial_into_buffer(Some(&wave_in), &mut wave_out, None)
                .map_err(|error| fail(format!("resample failed: {error}")))?;
            out.extend_from_slice(&wave_out[0][..produced]);
        } else {
            let (_, produced) = resampler
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .map_err(|error| fail(format!("resample failed: {error}")))?;
            out.extend_from_slice(&wave_out[0][..produced]);
        }
        if out.len() >= WINDOW_SAMPLES + resampler.output_delay() {
            break;
        }
    }
    // Push the filter's delayed tail out, then truncate to the window.
    let mut drained = 0usize;
    let delay = resampler.output_delay();
    while drained < delay {
        let mut wave_out = resampler.output_buffer_allocate(true);
        let produced = resampler
            .process_partial_into_buffer::<&[f64], Vec<f64>>(None, &mut wave_out, None)
            .map(|(_, produced)| produced)
            .unwrap_or(0);
        if produced == 0 {
            break;
        }
        out.extend_from_slice(&wave_out[0][..produced]);
        drained += produced;
    }
    out.truncate(WINDOW_SAMPLES);
    Ok(out)
}

/// `round(x * 32768)` clipped to s16, the spike's quantization.
fn quantize(mono_11k: &[f64]) -> Vec<i16> {
    mono_11k
        .iter()
        .map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}
