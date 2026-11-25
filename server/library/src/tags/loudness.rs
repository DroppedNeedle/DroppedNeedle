//! ReplayGain 2.0 analysis: EBU R128 integrated loudness (ITU-R BS.1770
//! K-weighting, 400 ms blocks every 100 ms, the -70 LUFS absolute gate
//! and the -10 LU relative gate) against the -18 LUFS reference, and the
//! sample peak. The numbers are written the way
//! loudgain writes them (`-6.52 dB`, `0.988525`).
//!
//! Files are decoded one packet at a time, so memory stays flat however
//! long a track is. An album is the gated loudness over every block of
//! all its tracks, as R128 album gain is defined.

use std::time::{Duration, Instant};

use super::{TrackFile, format_for_path};

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

    /// About how many seconds of audio were measured.
    pub fn seconds(&self) -> f64 {
        match self.blocks.len() {
            0 => 0.0,
            blocks => (blocks + 3) as f64 / 10.0,
        }
    }

    /// The blocks summed into loudness buckets, which is what is kept.
    pub fn summary(&self) -> BlockSummary {
        let mut summary = BlockSummary::default();
        for energy in &self.blocks {
            summary.add(*energy);
        }
        summary
    }

    /// Where the sound starts, fades out and ends, worked out from the
    /// blocks in order against the track's own loudness, the way
    /// Liquidsoap's autocue does. `None` for a track under 10 seconds, one
    /// with nothing above the floor, or one whose sound would end before
    /// it starts.
    pub fn edges(&self) -> Option<SoundEdges> {
        let length_ms = (self.seconds() * 1000.0).round() as i64;
        if length_ms < SHORTEST_MS {
            return None;
        }
        let loudness = self.loudness()?;
        let floor = (loudness + EDGE_FLOOR_LU).max(ABSOLUTE_GATE_LUFS);
        let first = self
            .blocks
            .iter()
            .position(|energy| lufs(*energy) > floor)?;
        let last = self
            .blocks
            .iter()
            .rposition(|energy| lufs(*energy) > floor)?;
        let block_end = |index: usize| index as i64 * BLOCK_STEP_MS + BLOCK_MS;
        let sound_start_ms = first as i64 * BLOCK_STEP_MS;
        let sound_end_ms = block_end(last).min(length_ms);
        if sound_start_ms >= sound_end_ms {
            return None;
        }
        let last_above = |lu: f64| {
            self.blocks[..=last]
                .iter()
                .rposition(|energy| lufs(*energy) > loudness + lu)
                .map_or(sound_start_ms, |index| block_end(index).min(sound_end_ms))
        };
        let mut fade_start_ms = last_above(FADE_LU);
        if sound_end_ms - fade_start_ms > LONG_FADE_MS {
            fade_start_ms = last_above(LONG_FADE_LU);
        }
        fade_start_ms = fade_start_ms.max(sound_end_ms - MOST_FADE_MS);
        if sound_end_ms - fade_start_ms < LEAST_FADE_MS {
            fade_start_ms = sound_end_ms;
        }
        Some(SoundEdges {
            sound_start_ms,
            fade_start_ms,
            sound_end_ms,
        })
    }
}

/// Where a track's sound starts, where its fade-out starts and where its
/// sound ends, in milliseconds from the start of the file. A track that
/// stops hard has its fade start where its sound ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundEdges {
    pub sound_start_ms: i64,
    pub fade_start_ms: i64,
    pub sound_end_ms: i64,
}

/// A track shorter than this has no edges.
const SHORTEST_MS: i64 = 10_000;
/// A block's length and the step between blocks.
const BLOCK_MS: i64 = 400;
const BLOCK_STEP_MS: i64 = 100;
/// Sound is anything above the track's loudness less this, and above the
/// absolute gate.
const EDGE_FLOOR_LU: f64 = -42.0;
/// The fade-out starts after the last block above the track's loudness
/// less this; when that leaves a fade longer than [`LONG_FADE_MS`], after
/// the last block above it less [`LONG_FADE_LU`].
const FADE_LU: f64 = -8.0;
const LONG_FADE_LU: f64 = -20.0;
const LONG_FADE_MS: i64 = 15_000;
/// A fade-out is never longer than this, and one shorter than
/// [`LEAST_FADE_MS`] is a hard stop.
const MOST_FADE_MS: i64 = 12_000;
const LEAST_FADE_MS: i64 = 1_000;

/// The ReplayGain gain in dB for a loudness in LUFS.
pub fn gain(loudness: f64) -> f64 {
    REFERENCE_LUFS - loudness
}

/// Format a gain the way ReplayGain readers expect.
pub fn gain_text(loudness: f64) -> String {
    format!("{:.2} dB", gain(loudness))
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

/// The lowest edge of the lowest bucket: the absolute gate.
const SUMMARY_FLOOR_LUFS: i32 = -70;
/// Buckets, 1 LU each, from the gate up to +20 LUFS; a louder block goes
/// in the top one.
const SUMMARY_BUCKETS: usize = 90;
/// The stored form's first byte, so a later form can be told apart.
const SUMMARY_VERSION: u8 = 1;
/// A stored bucket: its index, the block count and the summed energy.
const SUMMARY_ENTRY: usize = 1 + 4 + 8;

/// A file's 400 ms blocks above the absolute gate, summed into 1 LU
/// buckets by loudness. An album's loudness is worked out from its
/// tracks' summaries, within a few hundredths of an LU of gating every
/// block, so it is never measured or stored itself.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockSummary {
    buckets: [(u32, f64); SUMMARY_BUCKETS],
}

impl Default for BlockSummary {
    fn default() -> Self {
        Self {
            buckets: [(0, 0.0); SUMMARY_BUCKETS],
        }
    }
}

/// A stored summary that does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a loudness summary does not read")]
pub struct BadSummary;

impl BlockSummary {
    /// Count one block, unless the absolute gate drops it.
    fn add(&mut self, energy: f64) {
        if energy.is_nan() || energy <= 0.0 {
            return;
        }
        let level = lufs(energy);
        if level <= ABSOLUTE_GATE_LUFS {
            return;
        }
        let index =
            ((level - f64::from(SUMMARY_FLOOR_LUFS)).floor() as usize).min(SUMMARY_BUCKETS - 1);
        let bucket = &mut self.buckets[index];
        bucket.0 = bucket.0.saturating_add(1);
        bucket.1 += energy;
    }

    /// Add another track's blocks, for an album.
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.buckets.iter_mut().zip(&other.buckets) {
            mine.0 = mine.0.saturating_add(theirs.0);
            mine.1 += theirs.1;
        }
    }

    /// Integrated loudness in LUFS; `None` for silence. The relative gate
    /// keeps or drops each bucket whole, by the mean loudness of its
    /// blocks: exact for blocks of one level, and off by part of one
    /// bucket's blocks at most for blocks spread across it.
    pub fn loudness(&self) -> Option<f64> {
        let (count, energy) = self
            .buckets
            .iter()
            .fold((0u64, 0.0), |(count, energy), bucket| {
                (count + u64::from(bucket.0), energy + bucket.1)
            });
        if count == 0 {
            return None;
        }
        let threshold = lufs(energy / count as f64) + RELATIVE_GATE_LU;
        let (kept_count, kept_energy) = self
            .buckets
            .iter()
            .filter(|(count, energy)| *count > 0 && lufs(energy / f64::from(*count)) > threshold)
            .fold((0u64, 0.0), |(count, energy), bucket| {
                (count + u64::from(bucket.0), energy + bucket.1)
            });
        (kept_count > 0).then(|| lufs(kept_energy / kept_count as f64))
    }

    /// The stored form: a version byte, then each bucket holding blocks as
    /// its index, block count and summed energy, little-endian.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![SUMMARY_VERSION];
        for (index, (count, energy)) in self.buckets.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            bytes.push(index as u8);
            bytes.extend_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&energy.to_le_bytes());
        }
        bytes
    }

    /// Read the stored form back. Anything [`encode`](Self::encode) does
    /// not write is refused.
    pub fn decode(bytes: &[u8]) -> Result<Self, BadSummary> {
        let (&version, entries) = bytes.split_first().ok_or(BadSummary)?;
        if version != SUMMARY_VERSION || entries.len() % SUMMARY_ENTRY != 0 {
            return Err(BadSummary);
        }
        let mut summary = Self::default();
        let mut next = 0usize;
        for entry in entries.as_chunks::<SUMMARY_ENTRY>().0 {
            let index = usize::from(entry[0]);
            let count = u32::from_le_bytes(entry[1..5].try_into().map_err(|_| BadSummary)?);
            let energy = f64::from_le_bytes(entry[5..13].try_into().map_err(|_| BadSummary)?);
            if index < next
                || index >= SUMMARY_BUCKETS
                || count == 0
                || !energy.is_finite()
                || energy <= 0.0
            {
                return Err(BadSummary);
            }
            summary.buckets[index] = (count, energy);
            next = index + 1;
        }
        Ok(summary)
    }
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
    /// The file could not be opened or read to its end: it may be
    /// mid-copy, or its folder may be failing. Worth another try later.
    Unreadable,
    /// The time allowed ran out.
    OutOfTime,
    /// The caller asked it to stop.
    Stopped,
}

/// Measure one file, giving up at `deadline`.
pub fn analyse(at: TrackFile<'_>, deadline: Instant) -> Result<TrackLoudness, Unanalysed> {
    analyse_until(at, deadline, &|| false)
}

/// [`analyse`], also giving up once `stop` answers true. Both are checked
/// every 64 packets.
pub fn analyse_until(
    at: TrackFile<'_>,
    deadline: Instant,
    stop: &dyn Fn() -> bool,
) -> Result<TrackLoudness, Unanalysed> {
    let format = format_for_path(at.path()).map_err(|_| Unanalysed::Undecodable)?;
    let mut meter: Option<Meter> = None;
    let mut ended: Option<Unanalysed> = None;
    let mut reshaped = false;
    let mut packets = 0u32;
    let decoded = super::probe::decode_stream(at, format, |rate, channels, block| {
        let meter = meter.get_or_insert_with(|| Meter::new(rate, channels as usize));
        if meter.filters.len() != channels as usize || meter.rate != rate {
            // A stream that changes shape mid-file is not measured.
            reshaped = true;
            return false;
        }
        meter.feed(block);
        packets += 1;
        if packets.is_multiple_of(64) {
            if stop() {
                ended = Some(Unanalysed::Stopped);
                return false;
            }
            if Instant::now() > deadline {
                ended = Some(Unanalysed::OutOfTime);
                return false;
            }
        }
        true
    });
    if let Some(ended) = ended {
        return Err(ended);
    }
    // A stream that stopped early (a decode or read error on the way) is
    // not measured: its numbers would describe part of the track.
    match (decoded, meter) {
        (Ok(end), Some(meter)) if !reshaped && !end.cut_short && !end.wobbled => Ok(meter.finish()),
        (Ok(end), _) if end.cut_short && !end.wobbled => Err(Unanalysed::Unreadable),
        (Err(super::TagsError::Io { .. }), _) => Err(Unanalysed::Unreadable),
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
pub fn analyse_cached(at: TrackFile<'_>, deadline: Instant) -> Result<TrackLoudness, Unanalysed> {
    let sha = file_sha256(at).map_err(|_| Unanalysed::Unreadable)?;
    if let Ok(cache) = MEASURED.lock()
        && let Some(found) = cache.as_ref().and_then(|cache| cache.by_sha.get(&sha))
    {
        return Ok(TrackLoudness::clone(found));
    }
    let found = analyse(at, deadline)?;
    // The file must not have changed while it was decoded.
    if file_sha256(at).ok().as_deref() == Some(sha.as_str())
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

fn file_sha256(at: TrackFile<'_>) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = at.open()?;
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

    /// The meter reads a reference tone as R128 says and silence as no
    /// loudness, and analysis decodes each writable format: the fixtures
    /// are digital silence, so they measure no loudness and no peak (a
    /// lossy decoder may leave float dust far below one 24-bit step).
    #[test]
    fn loudness_measures_as_r128_says() {
        // EBU Tech 3341: a 997 Hz stereo sine at -23 dBFS reads -23 LUFS.
        let amplitude = 10f32.powf(-23.0 / 20.0);
        let mut meter = Meter::new(48_000, 2);
        meter.feed(&sine(48_000, 10.0, amplitude));
        let measured = meter.finish();
        let loudness = measured.loudness().expect("not silent");
        assert!((loudness - -23.0).abs() < 0.1, "{loudness}");
        assert_eq!(gain_text(loudness), "5.00 dB");
        assert!((measured.peak - f64::from(amplitude)).abs() < 1e-3);

        let mut meter = Meter::new(44_100, 2);
        meter.feed(&vec![0.0; 44_100 * 2 * 3]);
        assert_eq!(meter.finish().loudness(), None, "silence");

        let deadline = Instant::now() + Duration::from_secs(60);
        for name in [
            "management_full.flac",
            "management_full.mp3",
            "management_full.ogg",
            "management_full.opus",
            "management_full.m4a",
        ] {
            let fixtures =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/library");
            let path = fixtures.join(name);
            let measured = analyse(TrackFile::new(&fixtures, &path), deadline).expect(name);
            assert_eq!(measured.loudness(), None, "{name}");
            assert!(measured.peak < 1e-9, "{name}: {}", measured.peak);
            assert_eq!(peak_text(measured.peak), "0.000000", "{name}");
        }
    }

    /// An album's loudness worked out from its tracks' stored buckets is
    /// the loudness of every block gated together, within 0.1 LU, even when
    /// a quiet passage or a fade sits at the relative gate. The stored form reads
    /// back as it was written, and anything else is refused.
    #[test]
    fn album_loudness_from_buckets_matches_every_block() {
        let measure = |parts: &[(f64, f32)]| {
            let mut meter = Meter::new(48_000, 2);
            for (seconds, dbfs) in parts {
                meter.feed(&sine(48_000, *seconds, 10f32.powf(dbfs / 20.0)));
            }
            meter.finish()
        };
        let loud = measure(&[(20.0, -16.0), (10.0, -29.5), (15.0, -17.0)]);
        // A fade spreads blocks over the buckets around the gate.
        let fade: Vec<(f64, f32)> = (0..40)
            .map(|step| (0.5, -22.0 - step as f32 * 0.5))
            .collect();
        let soft = measure(&[&[(30.0, -24.0)][..], &fade, &[(5.0, -60.0)]].concat());
        let album = [&loud, &soft];
        let exact = album_loudness(&album).expect("not silent");
        let mut summed = loud.summary();
        summed.merge(&soft.summary());
        let from_buckets = summed.loudness().expect("not silent");
        assert!(
            (from_buckets - exact).abs() < 0.1,
            "{from_buckets} against {exact}"
        );
        let track = loud.summary().loudness().expect("not silent");
        let track_exact = loud.loudness().expect("not silent");
        assert!(
            (track - track_exact).abs() < 0.1,
            "{track} against {track_exact}"
        );

        let stored = summed.encode();
        assert_eq!(BlockSummary::decode(&stored), Ok(summed.clone()));
        let silent = measure(&[(3.0, -100.0)]).summary();
        assert_eq!(silent.loudness(), None);
        assert_eq!(BlockSummary::decode(&silent.encode()), Ok(silent));
        let mut out_of_order = stored.clone();
        out_of_order[1] = 89;
        let mut unknown_bucket = stored.clone();
        unknown_bucket[1] = 90;
        for bad in [
            Vec::new(),
            [&[2u8][..], &stored[1..]].concat(),
            stored[..stored.len() - 1].to_vec(),
            out_of_order,
            unknown_bucket,
        ] {
            assert_eq!(BlockSummary::decode(&bad), Err(BadSummary), "{bad:?}");
        }
    }

    /// Where a track's sound starts, fades out and ends: a fade-out tail
    /// starts its fade where the level drops 8 LU under the track's and
    /// ends where it drops 42 under; a hard stop has no fade; leading
    /// silence is not sound; a long quiet tail fades for 12 seconds at
    /// most; a track under 10 seconds, or silent, has no edges.
    #[test]
    fn sound_edges_follow_the_level_against_the_tracks_loudness() {
        // Each part: seconds, then the level in LUFS it starts and ends at.
        let shaped = |parts: &[(f64, f64, f64)]| {
            let mut meter = Meter::new(48_000, 2);
            for (seconds, from, to) in parts {
                let steps = (seconds * 10.0).round() as usize;
                for step in 0..steps {
                    let level = from + (to - from) * step as f64 / steps as f64;
                    meter.sub_blocks.push(10f64.powf((level + 0.691) / 10.0));
                }
            }
            meter.finish().edges()
        };
        let near = |found: i64, expected: i64| (found - expected).abs() <= 500;
        let silence = -120.0;

        let fade_out = shaped(&[
            (2.0, silence, silence),
            (60.0, -14.0, -14.0),
            (12.0, -14.0, -62.0),
            (3.0, silence, silence),
        ])
        .expect("edges");
        assert!(near(fade_out.sound_start_ms, 2_000), "{fade_out:?}");
        assert!(near(fade_out.fade_start_ms, 64_400), "{fade_out:?}");
        assert!(near(fade_out.sound_end_ms, 72_900), "{fade_out:?}");

        let hard_stop = shaped(&[
            (1.0, silence, silence),
            (30.0, -12.0, -12.0),
            (2.0, silence, silence),
        ])
        .expect("edges");
        assert!(near(hard_stop.sound_end_ms, 31_000), "{hard_stop:?}");
        assert_eq!(hard_stop.fade_start_ms, hard_stop.sound_end_ms);

        let late_start = shaped(&[(8.0, silence, silence), (20.0, -14.0, -14.0)]).expect("edges");
        assert!(near(late_start.sound_start_ms, 8_000), "{late_start:?}");

        let quiet_tail = shaped(&[
            (40.0, -10.0, -10.0),
            (30.0, -20.0, -50.0),
            (2.0, silence, silence),
        ])
        .expect("edges");
        assert!(near(quiet_tail.sound_end_ms, 70_000), "{quiet_tail:?}");
        assert_eq!(
            quiet_tail.sound_end_ms - quiet_tail.fade_start_ms,
            12_000,
            "{quiet_tail:?}"
        );

        assert_eq!(shaped(&[(9.0, -14.0, -14.0)]), None, "under 10 seconds");
        assert_eq!(shaped(&[(30.0, silence, silence)]), None, "silence");
    }

    /// Bytes after a FLAC file's last frame, an ID3v1 tag or a stale copy
    /// of an earlier frame a tagger left behind, cost the reader only that
    /// frame, so the file is still measured. A file cut off before its last
    /// frame is not: it reads as unreadable, as a file mid-copy does.
    #[test]
    fn bytes_after_the_last_frame_are_not_audio_cut_short() {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/library");
        let plain = std::fs::read(fixtures.join("management_full.flac")).unwrap();
        let scratch = droppedneedle_kernel::test_support::ScratchDir::new("loudness-tail")
            .expect("scratch dir");
        let deadline = Instant::now() + Duration::from_secs(60);
        let measure = |bytes: &[u8]| {
            let path = scratch.path().join("track.flac");
            std::fs::write(&path, bytes).unwrap();
            analyse(TrackFile::new(scratch.path(), &path), deadline)
        };
        let frame_starts: Vec<usize> = (0..plain.len() - 1)
            .filter(|&at| plain[at] == 0xFF && plain[at + 1] == 0xF8)
            .collect();
        let [.., before_last, last] = frame_starts[..] else {
            panic!("the fixture has two frames or more");
        };

        let mut id3v1 = b"TAG".to_vec();
        id3v1.resize(128, b' ');
        assert!(measure(&[&plain[..], &id3v1].concat()).is_ok());
        let stale = &plain[before_last..before_last + (last - before_last) / 2];
        assert!(measure(&[&plain[..], stale].concat()).is_ok());
        assert_eq!(measure(&plain[..before_last]), Err(Unanalysed::Unreadable));
    }
}
