//! One track's sound: its audio, prepared as [`super::front`] says, through
//! the model one patch at a time. The track vector is the plain mean of
//! every patch's 200 values (not normalised); a tag's score is the mean
//! over patches of that patch's sigmoid, kept as a byte.

use std::time::Instant;

use super::front::{FrontError, MOST_PATCHES, MOST_SECONDS, MelStream, Resample};
use super::net::{EMBEDDING, ModelError, Models, TAG_COUNT};
use crate::tags::TrackFile;

/// What a track sounds like.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSound {
    /// The mean of every patch's 200 values.
    pub vector: Vec<f32>,
    /// Each of the 50 tags' scores, 0 to 255.
    pub tags: Vec<u8>,
    /// Patches analysed.
    pub patches: usize,
    /// Seconds of audio analysed (at most [`MOST_SECONDS`]).
    pub seconds: f64,
}

impl TrackSound {
    /// The vector as stored: 200 little-endian 32-bit floats.
    #[must_use]
    pub fn vector_bytes(&self) -> Vec<u8> {
        self.vector
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }
}

/// Why a file gave no sound.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Unanalysed {
    /// It could not be decoded, or stopped early.
    #[error("the file would not decode")]
    Undecodable,
    /// Shorter than one patch, about 3 seconds.
    #[error("the file is shorter than 3 seconds")]
    TooShort,
    /// It took longer than a file may.
    #[error("the file took too long to read")]
    OverTime,
    /// Asked to stop.
    #[error("stopped")]
    Stopped,
    /// The model or the preparation failed, whatever the file.
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Front(#[from] FrontError),
}

/// Mono samples at one rate in, a [`TrackSound`] out, holding only the
/// running sums and the samples not yet framed.
pub struct Analyser<'a> {
    models: &'a Models,
    rate: u32,
    resample: Resample,
    mel: MelStream,
    resampled: Vec<f32>,
    sums: Vec<f64>,
    tag_sums: Vec<f64>,
    patches: usize,
    frames: u64,
    failed: Option<ModelError>,
}

impl<'a> Analyser<'a> {
    /// An analyser for a stream at `rate`.
    pub fn new(models: &'a Models, rate: u32) -> Result<Self, FrontError> {
        Ok(Self {
            models,
            rate,
            resample: Resample::new(rate)?,
            mel: MelStream::new(),
            resampled: Vec::new(),
            sums: vec![0.0; EMBEDDING],
            tag_sums: vec![0.0; TAG_COUNT],
            patches: 0,
            frames: 0,
            failed: None,
        })
    }

    /// The stream's rate.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Whether the first [`MOST_SECONDS`] have been taken: anything fed
    /// after is left out.
    #[must_use]
    pub fn full(&self) -> bool {
        self.frames >= u64::from(MOST_SECONDS) * u64::from(self.rate)
    }

    /// Feed mono samples, up to the first [`MOST_SECONDS`] of the stream.
    pub fn feed(&mut self, mono: &[f32]) -> Result<(), Unanalysed> {
        let room = (u64::from(MOST_SECONDS) * u64::from(self.rate)).saturating_sub(self.frames);
        let take = &mono[..mono.len().min(usize::try_from(room).unwrap_or(usize::MAX))];
        self.frames += take.len() as u64;
        self.resampled.clear();
        self.resample.push(take, &mut self.resampled)?;
        self.frame()
    }

    fn frame(&mut self) -> Result<(), Unanalysed> {
        let (models, sums, tag_sums, patches, failed) = (
            self.models,
            &mut self.sums,
            &mut self.tag_sums,
            &mut self.patches,
            &mut self.failed,
        );
        self.mel.push(&self.resampled, &mut |patch| {
            if *patches >= MOST_PATCHES {
                return false;
            }
            match embed(models, patch, sums, tag_sums) {
                Ok(()) => {
                    *patches += 1;
                    true
                }
                Err(error) => {
                    *failed = Some(error);
                    false
                }
            }
        })?;
        match self.failed.take() {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    /// The stream ended: the resampler's tail, then the result. A stream
    /// shorter than one patch is [`Unanalysed::TooShort`].
    pub fn finish(mut self) -> Result<TrackSound, Unanalysed> {
        self.resampled.clear();
        self.resample.finish(&mut self.resampled)?;
        self.frame()?;
        if self.patches == 0 {
            return Err(Unanalysed::TooShort);
        }
        if !self
            .sums
            .iter()
            .chain(&self.tag_sums)
            .all(|sum| sum.is_finite())
        {
            return Err(Unanalysed::Undecodable);
        }
        let count = self.patches as f64;
        Ok(TrackSound {
            vector: self.sums.iter().map(|sum| (sum / count) as f32).collect(),
            tags: self
                .tag_sums
                .iter()
                .map(|sum| (sum / count * 255.0).round().clamp(0.0, 255.0) as u8)
                .collect(),
            patches: self.patches,
            seconds: self.frames as f64 / f64::from(self.rate),
        })
    }
}

fn embed(
    models: &Models,
    patch: &[f32],
    sums: &mut [f64],
    tag_sums: &mut [f64],
) -> Result<(), ModelError> {
    let values = models.embed(patch)?;
    let logits = models.tag_logits(&values)?;
    for (sum, value) in sums.iter_mut().zip(&values) {
        *sum += f64::from(*value);
    }
    for (sum, logit) in tag_sums.iter_mut().zip(&logits) {
        *sum += 1.0 / (1.0 + (-f64::from(*logit)).exp());
    }
    Ok(())
}

/// The sound of the file `at`, decoded packet by packet with the codecs the
/// app reads tags and loudness with. Gives up past `deadline` or once
/// `stop` answers true, both checked every 16 packets. A stream that stops
/// early, changes rate or does not decode cleanly is undecodable, as it is
/// for the loudness measure. Blocking.
pub fn analyse_file(
    at: TrackFile<'_>,
    models: &Models,
    deadline: Instant,
    stop: &dyn Fn() -> bool,
) -> Result<TrackSound, Unanalysed> {
    let format = crate::tags::format_for_path(at.path()).map_err(|_| Unanalysed::Undecodable)?;
    let mut analyser: Option<Analyser<'_>> = None;
    let mut ended: Option<Unanalysed> = None;
    let mut mono: Vec<f32> = Vec::new();
    let mut packets = 0u32;
    let decoded = crate::tags::probe::decode_stream(at, format, |rate, channels, block| {
        let current = match analyser.as_mut() {
            Some(current) => current,
            None => match Analyser::new(models, rate) {
                Ok(started) => analyser.insert(started),
                Err(error) => {
                    ended = Some(error.into());
                    return false;
                }
            },
        };
        if current.rate() != rate {
            ended = Some(Unanalysed::Undecodable);
            return false;
        }
        let channels = channels.max(1) as usize;
        mono.clear();
        mono.extend(
            block
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        if let Err(error) = current.feed(&mono) {
            ended = Some(error);
            return false;
        }
        packets += 1;
        if packets.is_multiple_of(16) {
            if stop() {
                ended = Some(Unanalysed::Stopped);
                return false;
            }
            if Instant::now() > deadline {
                ended = Some(Unanalysed::OverTime);
                return false;
            }
        }
        !current.full()
    });
    if let Some(ended) = ended {
        return Err(ended);
    }
    match (decoded, analyser) {
        (Ok(end), Some(analyser)) if !end.cut_short && !end.wobbled => analyser.finish(),
        _ => Err(Unanalysed::Undecodable),
    }
}
