//! The audio preparation in front of the model, value for value as the
//! reference (librosa 1.0.0 with `soxr_hq`) prepares it, streamed so memory
//! stays flat whatever the length: the mean of every channel, resampled to
//! 16 kHz, 512-point frames every 256 samples with no centre padding, 96
//! Slaney mel bands, `log10(1 + 10000 x)`, and non-overlapping 187-frame
//! patches, at most [`MOST_PATCHES`] of them.
//!
//! Each stage was checked against the reference on 2026-11-27 (largest
//! log-mel difference 9.5e-7 on 20 real files); the golden tests in
//! `server/tests/library/library_sound.rs` hold it. A change to anything
//! here changes the vectors: bump [`super::ANALYSIS_VERSION`] with it.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// The model's sample rate.
pub const RATE: u32 = 16_000;
const N_FFT: usize = 512;
const HOP: usize = 256;
/// Mel bands, the model's second input dimension.
pub const N_MELS: usize = 96;
/// Frames in one patch, the model's first input dimension.
pub const PATCH_FRAMES: usize = 187;
/// Values in one patch.
pub const PATCH_LEN: usize = PATCH_FRAMES * N_MELS;
/// Seconds of a track that are analysed: 200 patches.
pub const MOST_SECONDS: u32 = 600;
/// Patches a track gives at most.
pub const MOST_PATCHES: usize = 200;
/// Input frames handed to the resampler at a time.
const CHUNK: usize = 4096;

/// A failure in the preparation itself (not the file): the resampler or
/// the FFT refused its own buffers.
#[derive(Debug, Clone, thiserror::Error)]
#[error("audio preparation: {0}")]
pub struct FrontError(pub String);

/// The resampler's settings for one input rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SincSetting {
    /// Filter length in input samples.
    pub sinc_len: usize,
    /// Cutoff as a share of the output's Nyquist frequency.
    pub f_cutoff: f32,
}

/// The resampler's cutoff at every rate.
const SINC_CUTOFF: f32 = 0.957;

/// The settings for an input rate, fitted per rate against `soxr_hq` by
/// the track vector's cosine to the reference's. The filter length counts
/// input samples, so one length fitted at 44.1 kHz is a filter four times
/// as coarse at 192 kHz, and the high rates fell short: the length is kept
/// the same in time instead, 512 samples at 44.1 kHz, so the filter's edge
/// sits where soxr's does at every rate. A rate at or below 44.1 kHz keeps
/// 512.
///
/// Fitted on 2026-11-27 on the dev library's FLAC files, the first 120
/// seconds of 12 to 14 tracks a rate, over lengths and cutoffs from 0.95 to
/// 0.965: the cutoff 0.957 was best or within 0.00003 of best at every rate.
/// The lengths it gives: 512 at 44.1 kHz, 560 at 48 kHz, 1024 at 88.2 kHz,
/// 1120 at 96 kHz and 2232 at 192 kHz. Checked on whole files (up to 600
/// seconds) the same day, every high-rate FLAC there and 100 at 44.1 kHz,
/// worst track vector cosine to the reference: 44.1 kHz 0.99976 (100
/// files), 48 kHz 0.99985 (94), 88.2 kHz 0.99989 (16), 96 kHz 0.99979
/// (124), 192 kHz 0.99992 (94); none under 0.999. With 512 and 0.955 at
/// every rate, 1 of 110 at 48 kHz, 4 of 16 at 88.2 kHz, 23 of 124 at 96 kHz
/// and 38 of 94 at 192 kHz were under it.
#[must_use]
pub fn sinc_setting(rate: u32) -> SincSetting {
    let sinc_len = if rate <= 44_100 {
        512
    } else {
        let len = (512.0 * f64::from(rate) / 44_100.0).round() as usize;
        len.div_ceil(8) * 8
    };
    SincSetting {
        sinc_len,
        f_cutoff: SINC_CUTOFF,
    }
}

/// Mono samples at the file's rate in, 16 kHz out. rubato's
/// `SincFixedIn` with the rate's [`sinc_setting`], cubic interpolation,
/// oversampling 256 and a Blackman-Harris squared window. Its output delay
/// is kept, not skipped: the reference's soxr has none to remove, and
/// skipping it moves the signal early. The output is cut to
/// `ceil(n x 16000 / rate)` samples, the length soxr gives.
pub struct Resample {
    sinc: Option<SincFixedIn<f32>>,
    rate: u32,
    inbuf: Vec<f32>,
    out: Vec<Vec<f32>>,
    taken: u64,
    given: u64,
}

impl Resample {
    /// A resampler for a stream at `rate`. At the model's own rate the
    /// samples pass through.
    pub fn new(rate: u32) -> Result<Self, FrontError> {
        let sinc = if rate == RATE {
            None
        } else {
            let setting = sinc_setting(rate);
            let params = SincInterpolationParameters {
                sinc_len: setting.sinc_len,
                f_cutoff: setting.f_cutoff,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(f64::from(RATE) / f64::from(rate), 1.0, params, CHUNK, 1)
                    .map_err(|error| FrontError(error.to_string()))?,
            )
        };
        let out = match &sinc {
            Some(sinc) => sinc.output_buffer_allocate(true),
            None => vec![Vec::new()],
        };
        Ok(Self {
            sinc,
            rate,
            inbuf: Vec::with_capacity(CHUNK * 2),
            out,
            taken: 0,
            given: 0,
        })
    }

    /// The samples this stream gives in all, as soxr gives them.
    fn wanted(&self) -> u64 {
        (self.taken * u64::from(RATE)).div_ceil(u64::from(self.rate))
    }

    /// Hand on the tail's `produced` samples, up to `cap` given in all.
    fn give(&mut self, produced: usize, dst: &mut Vec<f32>, cap: u64) {
        let room = usize::try_from(cap.saturating_sub(self.given)).unwrap_or(usize::MAX);
        let slice = &self.out[0][..produced.min(room)];
        self.given += slice.len() as u64;
        dst.extend_from_slice(slice);
    }

    /// Feed `mono`; whatever 16 kHz samples are ready go on `dst`.
    pub fn push(&mut self, mono: &[f32], dst: &mut Vec<f32>) -> Result<(), FrontError> {
        self.taken += mono.len() as u64;
        let Some(sinc) = self.sinc.as_mut() else {
            dst.extend_from_slice(mono);
            self.given += mono.len() as u64;
            return Ok(());
        };
        self.inbuf.extend_from_slice(mono);
        let mut start = 0;
        loop {
            let need = sinc.input_frames_next();
            if self.inbuf.len() - start < need {
                break;
            }
            let (used, produced) = sinc
                .process_into_buffer(&[&self.inbuf[start..start + need]], &mut self.out, None)
                .map_err(|error| FrontError(error.to_string()))?;
            start += used;
            self.given += produced as u64;
            dst.extend_from_slice(&self.out[0][..produced]);
        }
        self.inbuf.drain(..start);
        Ok(())
    }

    /// The stream ended: the samples still held, and the filter's tail,
    /// up to the length soxr gives.
    pub fn finish(&mut self, dst: &mut Vec<f32>) -> Result<(), FrontError> {
        let wanted = self.wanted();
        if self.sinc.is_none() {
            return Ok(());
        }
        let rest = std::mem::take(&mut self.inbuf);
        let mut first = true;
        // The tail comes out over a few calls; each gives a chunk or more.
        for _ in 0..64 {
            if self.given >= wanted {
                break;
            }
            let input: Option<&[&[f32]]> = if first && !rest.is_empty() {
                Some(&[&rest[..]])
            } else {
                None
            };
            first = false;
            let Some(sinc) = self.sinc.as_mut() else {
                break;
            };
            let (_, produced) = sinc
                .process_partial_into_buffer(input, &mut self.out, None)
                .map_err(|error| FrontError(error.to_string()))?;
            self.give(produced, dst, wanted);
        }
        Ok(())
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if hz >= min_log_hz {
        min_log_mel + (hz / min_log_hz).ln() / logstep
    } else {
        hz / f_sp
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if mel >= min_log_mel {
        min_log_hz * (logstep * (mel - min_log_mel)).exp()
    } else {
        f_sp * mel
    }
}

/// librosa's Slaney mel filter bank, 0 to 8 kHz with Slaney's area
/// normalisation, as each band's first FFT bin and its weights. Built in
/// f64 and rounded to f32, as the reference's is.
fn mel_bank() -> Vec<(usize, Vec<f32>)> {
    let bins = N_FFT / 2 + 1;
    let rate = f64::from(RATE);
    let bin_hz: Vec<f64> = (0..bins)
        .map(|bin| bin as f64 * rate / N_FFT as f64)
        .collect();
    let (low, high) = (hz_to_mel(0.0), hz_to_mel(rate / 2.0));
    let edges: Vec<f64> = (0..N_MELS + 2)
        .map(|at| mel_to_hz(low + (high - low) * at as f64 / (N_MELS + 1) as f64))
        .collect();
    (0..N_MELS)
        .map(|band| {
            let rise = edges[band + 1] - edges[band];
            let fall = edges[band + 2] - edges[band + 1];
            let norm = 2.0 / (edges[band + 2] - edges[band]);
            let weights: Vec<f32> = bin_hz
                .iter()
                .map(|&hz| {
                    let lower = -(edges[band] - hz) / rise;
                    let upper = (edges[band + 2] - hz) / fall;
                    (0f64.max(lower.min(upper)) * norm) as f32
                })
                .collect();
            let first = weights.iter().position(|&w| w != 0.0).unwrap_or(0);
            let last = weights
                .iter()
                .rposition(|&w| w != 0.0)
                .map_or(first, |at| at + 1);
            (first, weights[first..last].to_vec())
        })
        .collect()
}

/// 16 kHz samples in, log-mel patches out: a periodic Hann window, a
/// 512-point FFT in f64, the power spectrum, the mel bands and the log,
/// frame after frame, and a patch every 187 frames.
pub struct MelStream {
    held: Vec<f32>,
    window: Vec<f64>,
    bank: Vec<(usize, Vec<f32>)>,
    fft: Arc<dyn RealToComplex<f64>>,
    frame: Vec<f64>,
    spectrum: Vec<Complex<f64>>,
    power: Vec<f32>,
    patch: Vec<f32>,
}

impl Default for MelStream {
    fn default() -> Self {
        Self::new()
    }
}

impl MelStream {
    #[must_use]
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
        let window = (0..N_FFT)
            .map(|at| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * at as f64 / N_FFT as f64).cos())
            .collect();
        Self {
            held: Vec::with_capacity(CHUNK * 2),
            window,
            bank: mel_bank(),
            frame: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            fft,
            power: vec![0.0; N_FFT / 2 + 1],
            patch: Vec::with_capacity(PATCH_LEN),
        }
    }

    /// Feed 16 kHz samples; each patch completed is handed to `patch` as
    /// 187 frames of 96 bands, which answers false to stop early.
    pub fn push(
        &mut self,
        samples: &[f32],
        patch: &mut dyn FnMut(&[f32]) -> bool,
    ) -> Result<bool, FrontError> {
        self.held.extend_from_slice(samples);
        let mut at = 0;
        let mut go_on = true;
        while go_on && at + N_FFT <= self.held.len() {
            for (slot, (sample, weight)) in self
                .frame
                .iter_mut()
                .zip(self.held[at..at + N_FFT].iter().zip(&self.window))
            {
                *slot = f64::from(*sample) * weight;
            }
            self.fft
                .process(&mut self.frame, &mut self.spectrum)
                .map_err(|error| FrontError(error.to_string()))?;
            for (power, bin) in self.power.iter_mut().zip(&self.spectrum) {
                *power = (bin.re * bin.re + bin.im * bin.im) as f32;
            }
            for (first, weights) in &self.bank {
                let energy: f32 = weights
                    .iter()
                    .zip(&self.power[*first..])
                    .map(|(weight, power)| weight * power)
                    .sum();
                self.patch.push((1.0 + 10_000.0 * energy.max(0.0)).log10());
            }
            if self.patch.len() == PATCH_LEN {
                go_on = patch(&self.patch);
                self.patch.clear();
            }
            at += HOP;
        }
        self.held.drain(..at);
        Ok(go_on)
    }
}
