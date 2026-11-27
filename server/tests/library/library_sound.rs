//! Sound analysis against its reference, stage by stage.
//!
//! The reference is AudioMuse-AI's front end, written again from its
//! parameters in Python (librosa 1.0.0, `soxr_hq` from soxr 1.1.0, ONNX
//! Runtime 1.28.0 on our own model export) and run once on 2026-11-27; its
//! outputs are the files in `tests/fixtures/library/sound/`. The test tone is
//! the same formula on both sides ([`tone`]); the two FLAC fixtures are
//! generated tones (no recorded music), encoded with the image's ffmpeg
//! 9.0.2. Each bar is the one the port was first checked against, with what
//! it measured on 20 real files in brackets: log-mel within 1e-3 (9.5e-7); the network on fixed patches at
//! cosine above 0.9999 and within 1e-4 (0.9999999999993, 8.3e-6); the
//! resampler, at each rate's setting, a track vector at cosine above 0.999;
//! end to end above 0.995 (0.99917 at worst).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use droppedneedle_library::sound::front::{MelStream, PATCH_LEN};
use droppedneedle_library::sound::{Analyser, Models, analyse_file};
use droppedneedle_library::tags::TrackFile;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library")
}

fn golden(name: &str) -> Vec<f32> {
    let bytes = std::fs::read(fixtures().join("sound").join(name)).expect("golden file");
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (norm(a) * norm(b))
}

fn largest_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// Seven seconds of partials across the band, 7 to 8 kHz included where
/// the resampler's edge sits, and above it where the rate allows, each
/// slowly swelling. The reference computes the same sums in the same order.
fn tone(rate: u32) -> Vec<f32> {
    const PARTIALS: [f64; 18] = [
        110.0, 220.0, 330.0, 440.5, 554.37, 659.25, 987.77, 1318.5, 2093.0, 3520.0, 5274.0, 7040.0,
        7458.6, 7902.1, 9000.0, 12000.0, 18000.0, 31000.0,
    ];
    let rate_f = f64::from(rate);
    let samples = (rate_f * 7.0).round() as usize;
    (0..samples)
        .map(|at| {
            let t = at as f64 / rate_f;
            let mut sum = 0.0f64;
            for (k, f) in PARTIALS.iter().enumerate() {
                if *f >= rate_f / 2.0 {
                    continue;
                }
                let k = k as f64;
                let swell = 1.0 + 0.5 * (2.0 * std::f64::consts::PI * (0.5 + 0.37 * k) * t).sin();
                sum += (0.3 / (k + 1.0))
                    * swell
                    * (2.0 * std::f64::consts::PI * f * t + 0.1 * k).sin();
            }
            sum as f32
        })
        .collect()
}

fn sound_of(
    models: &Models,
    rate: u32,
    samples: &[f32],
) -> droppedneedle_library::sound::TrackSound {
    let mut analyser = Analyser::new(models, rate).expect("analyser");
    for block in samples.chunks(1152) {
        analyser.feed(block).expect("fed");
    }
    analyser.finish().expect("analysed")
}

/// The log-mel front end, fed a 16 kHz signal, gives the reference's patch.
#[test]
fn the_log_mel_patch_is_the_references() {
    let mut mel = MelStream::new();
    let mut first: Option<Vec<f32>> = None;
    mel.push(&tone(16_000), &mut |patch| {
        first.get_or_insert_with(|| patch.to_vec());
        true
    })
    .expect("framed");
    let ours = first.expect("a patch");
    let reference = golden("mel_patch.f32");
    assert_eq!(ours.len(), PATCH_LEN);
    let difference = largest_difference(&ours, &reference);
    assert!(
        difference < 1e-3,
        "largest log-mel difference {difference:e}"
    );
}

/// The embedding model, fed the reference's patch, gives the reference's
/// 200 values; the tag head on them gives its 50 scores.
#[test]
fn the_network_gives_the_references_outputs() {
    let models = Models::load().expect("models load");
    let patch = golden("mel_patch.f32");
    let values = models.embed(&patch).expect("embedded");
    let reference = golden("mel_patch_embedding.f32");
    let (similar, difference) = (
        cosine(&values, &reference),
        largest_difference(&values, &reference),
    );
    assert!(
        similar > 0.9999 && difference < 1e-4,
        "cosine {similar}, largest difference {difference:e}"
    );

    let tags: Vec<f32> = models
        .tag_logits(&reference)
        .expect("tagged")
        .iter()
        .map(|logit| (1.0 / (1.0 + (-f64::from(*logit)).exp())) as f32)
        .collect();
    let difference = largest_difference(&tags, &golden("mel_patch_tags.f32"));
    assert!(
        difference < 1e-4,
        "largest tag score difference {difference:e}"
    );
}

/// At each fitted rate, the resampler's setting brings the track vector to
/// the one `soxr_hq` gives. The tone is gentler than music: one 44.1 kHz
/// setting at every rate still reaches 0.99996 here (192 kHz), against
/// 0.9999995 with the fitted ones, so besides that 0.999 bar the tone is
/// held to 0.99999, which tells the two apart at 88.2, 96 and 192 kHz.
#[test]
fn each_rate_resamples_to_the_references_vector() {
    let models = Models::load().expect("models load");
    for rate in [44_100, 48_000, 88_200, 96_000, 192_000] {
        let sound = sound_of(&models, rate, &tone(rate));
        assert_eq!(sound.patches, 2, "{rate} Hz");
        let similar = cosine(&sound.vector, &golden(&format!("tone_{rate}.vec.f32")));
        assert!(similar > 0.99999, "{rate} Hz: cosine {similar}");
    }
}

/// From the file: decoded packet by packet, the channels averaged,
/// resampled, framed and run through the model, the vector and the tags
/// are the reference's.
#[test]
fn a_file_gives_the_references_vector_and_tags() {
    let models = Models::load().expect("models load");
    for name in ["sound_44k_stereo", "sound_96k_mono"] {
        let root = fixtures();
        let path = root.join(format!("{name}.flac"));
        let file = TrackFile::new(&root, &path);
        let sound = analyse_file(
            file,
            &models,
            Instant::now() + Duration::from_secs(60),
            &|| false,
        )
        .expect("analysed");
        assert_eq!(sound.patches, 1, "{name}");
        assert!(
            (sound.seconds - 4.2).abs() < 1e-6,
            "{name}: {} s",
            sound.seconds
        );
        let similar = cosine(&sound.vector, &golden(&format!("{name}.vec.f32")));
        assert!(similar > 0.995, "{name}: cosine {similar}");
        let tags: Vec<f32> = sound
            .tags
            .iter()
            .map(|byte| f32::from(*byte) / 255.0)
            .collect();
        let difference = largest_difference(&tags, &golden(&format!("{name}.tags.f32")));
        assert!(
            difference < 0.01,
            "{name}: largest tag difference {difference}"
        );
    }
}
