//! ReplayGain 2.0 analysis: EBU R128 integrated loudness (ITU-R BS.1770
//! K-weighting, 400 ms blocks every 100 ms, the -70 LUFS absolute gate
//! and the -10 LU relative gate) against the -18 LUFS reference, and the
//! sample peak. v2 ran loudgain for this; the numbers are written the way
//! loudgain writes them (`-6.52 dB`, `0.988525`).
//!
//! Files are decoded one packet at a time, so memory stays flat however
//! long a track is. An album is the gated loudness over every block of
//! all its tracks, as R128 album gain is defined.

use std::path::Path;
use std::time::{Duration, Instant};

use super::format_for_path;

/// ReplayGain 2.0 reference loudness.
const REFERENCE_LUFS: f64 = -18.0;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;

/// One file's loudness facts.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackLoudness {
    /// Mean K-weighted energy of each 400 ms block.
    blocks: Vec<f64>,
    /// Largest absolute sample, 1.0 being full scale.
    pub peak: f64,
}

impl TrackLoudness {
    /// Integrated loudness in LUFS; `None` for silence.
    pub fn loudness(&self) -> Option<f64> {
        integrated(&self.blocks)
    }
}

/// Format a gain the way ReplayGain readers expect.
pub fn gain_text(loudness: f64) -> String {
    format!("{:.2} dB", REFERENCE_LUFS - loudness)
}

/// Format a peak the way ReplayGain readers expect.
pub fn peak_text(peak: f64) -> String {
    format!("{:.6}", peak.max(0.0))
}

/// The album's integrated loudness over every track's blocks.
pub fn album_loudness(tracks: &[&TrackLoudness]) -> Option<f64> {
    let blocks: Vec<f64> = tracks
        .iter()
        .flat_map(|track| track.blocks.iter().copied())
        .collect();
    integrated(&blocks)
}

fn lufs(energy: f64) -> f64 {
    -0.691 + 10.0 * energy.log10()
}

fn integrated(blocks: &[f64]) -> Option<f64> {
    let gated: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|energy| *energy > 0.0 && lufs(*energy) > ABSOLUTE_GATE_LUFS)
        .collect();
    if gated.is_empty() {
        return None;
    }
    let mean = gated.iter().sum::<f64>() / gated.len() as f64;
    let threshold = lufs(mean) + RELATIVE_GATE_LU;
    let kept: Vec<f64> = gated
        .into_iter()
        .filter(|energy| lufs(*energy) > threshold)
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(lufs(kept.iter().sum::<f64>() / kept.len() as f64))
}

/// A direct-form-I biquad.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn run(&mut self, input: f64) -> f64 {
        let output = self.b[0] * input + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [input, self.x[0]];
        self.y = [output, self.y[0]];
        output
    }
}

/// BS.1770's two K-weighting stages at `rate`, as libebur128 derives
/// them for any sample rate.
fn k_weighting(rate: f64) -> (Biquad, Biquad) {
    let (f0, gain, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(gain / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Biquad::default()
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Biquad::default()
    };
    (shelf, high_pass)
}

/// BS.1770 channel weights: surrounds count 1.41, the LFE nothing.
fn channel_weight(channels: usize, index: usize) -> f64 {
    match (channels, index) {
        (6, 3) => 0.0,
        (6, 4 | 5) => 1.41,
        _ => 1.0,
    }
}

/// Streaming meter for one file.
struct Meter {
    rate: u32,
    filters: Vec<(Biquad, Biquad)>,
    sub_block: usize,
    filled: usize,
    energy: f64,
    sub_blocks: Vec<f64>,
    peak: f64,
}

impl Meter {
    fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate,
            filters: vec![k_weighting(f64::from(rate)); channels],
            sub_block: (rate as usize / 10).max(1),
            filled: 0,
            energy: 0.0,
            sub_blocks: Vec::new(),
            peak: 0.0,
        }
    }

    fn feed(&mut self, samples: &[f32]) {
        let channels = self.filters.len();
        for frame in samples.chunks_exact(channels) {
            let mut sum = 0.0;
            for (index, sample) in frame.iter().enumerate() {
                let value = f64::from(*sample);
                self.peak = self.peak.max(value.abs());
                let (shelf, high_pass) = &mut self.filters[index];
                let weighted = high_pass.run(shelf.run(value));
                sum += channel_weight(channels, index) * weighted * weighted;
            }
            self.energy += sum;
            self.filled += 1;
            if self.filled == self.sub_block {
                self.sub_blocks.push(self.energy / self.sub_block as f64);
                self.energy = 0.0;
                self.filled = 0;
            }
        }
    }

    fn finish(self) -> TrackLoudness {
        // 400 ms blocks every 100 ms: the mean of four sub-blocks.
        let blocks = self
            .sub_blocks
            .windows(4)
            .map(|window| window.iter().sum::<f64>() / 4.0)
            .collect();
        TrackLoudness {
            blocks,
            peak: self.peak,
        }
    }
}

/// Why a file was not analysed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unanalysed {
    /// The file could not be decoded.
    Undecodable,
    /// The time allowed for the album ran out.
    OutOfTime,
}

/// Measure one file, giving up at `deadline`.
pub fn analyse(path: &Path, deadline: Instant) -> Result<TrackLoudness, Unanalysed> {
    let format = format_for_path(path).map_err(|_| Unanalysed::Undecodable)?;
    let mut meter: Option<Meter> = None;
    let mut late = false;
    let mut reshaped = false;
    let mut packets = 0u32;
    let decoded = super::probe::decode_stream(path, format, |rate, channels, block| {
        let meter = meter.get_or_insert_with(|| Meter::new(rate, channels as usize));
        if meter.filters.len() != channels as usize || meter.rate != rate {
            // A stream that changes shape mid-file is not measured.
            reshaped = true;
            return false;
        }
        meter.feed(block);
        packets += 1;
        if packets % 64 == 0 && Instant::now() > deadline {
            late = true;
            return false;
        }
        true
    });
    if late {
        return Err(Unanalysed::OutOfTime);
    }
    // A stream that stopped early (a decode or read error on the way) is
    // not measured: its numbers would describe part of the track.
    match (decoded, meter) {
        (Ok(false), Some(meter)) if !reshaped => Ok(meter.finish()),
        _ => Err(Unanalysed::Undecodable),
    }
}

/// Most 100 ms blocks the measurement cache holds over all its files
/// (about 55 hours of audio, 16 MB).
const CACHE_BLOCKS: usize = 2_000_000;

struct Measured {
    by_sha: std::collections::HashMap<String, std::sync::Arc<TrackLoudness>>,
    order: std::collections::VecDeque<String>,
    blocks: usize,
}

static MEASURED: std::sync::Mutex<Option<Measured>> = std::sync::Mutex::new(None);

/// [`analyse`], remembering each file's result by its content hash, so a
/// file measured once (a preview, then its replan) is not decoded again.
pub fn analyse_cached(path: &Path, deadline: Instant) -> Result<TrackLoudness, Unanalysed> {
    let sha = file_sha256(path).map_err(|_| Unanalysed::Undecodable)?;
    if let Ok(cache) = MEASURED.lock()
        && let Some(found) = cache.as_ref().and_then(|cache| cache.by_sha.get(&sha))
    {
        return Ok(TrackLoudness::clone(found));
    }
    let found = analyse(path, deadline)?;
    // The file must not have changed while it was decoded.
    if file_sha256(path).ok().as_deref() == Some(sha.as_str())
        && let Ok(mut cache) = MEASURED.lock()
    {
        let cache = cache.get_or_insert_with(|| Measured {
            by_sha: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            blocks: 0,
        });
        if found.blocks.len() <= CACHE_BLOCKS && !cache.by_sha.contains_key(&sha) {
            while cache.blocks + found.blocks.len() > CACHE_BLOCKS {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                if let Some(gone) = cache.by_sha.remove(&oldest) {
                    cache.blocks -= gone.blocks.len();
                }
            }
            cache.blocks += found.blocks.len();
            cache.order.push_back(sha.clone());
            cache.by_sha.insert(sha, std::sync::Arc::new(found.clone()));
        }
    }
    Ok(found)
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Most time one album's analysis may take before it is deferred.
pub const ALBUM_BUDGET: Duration = Duration::from_secs(600);

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, seconds: f64, amplitude: f32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .flat_map(|index| {
                let value = amplitude
                    * (2.0 * std::f32::consts::PI * 997.0 * index as f32 / rate as f32).sin();
                [value, value]
            })
            .collect()
    }

    #[test]
    fn a_full_scale_tone_measures_as_r128_says() {
        // EBU Tech 3341: a 997 Hz stereo sine at -23 dBFS reads -23 LUFS.
        let amplitude = 10f32.powf(-23.0 / 20.0);
        let mut meter = Meter::new(48_000, 2);
        meter.feed(&sine(48_000, 10.0, amplitude));
        let measured = meter.finish();
        let loudness = measured.loudness().expect("not silent");
        assert!((loudness - -23.0).abs() < 0.1, "{loudness}");
        assert_eq!(gain_text(loudness), "5.00 dB");
        assert!((measured.peak - f64::from(amplitude)).abs() < 1e-3);
    }

    #[test]
    fn silence_has_no_loudness() {
        let mut meter = Meter::new(44_100, 2);
        meter.feed(&vec![0.0; 44_100 * 2 * 3]);
        assert_eq!(meter.finish().loudness(), None);
    }
}
