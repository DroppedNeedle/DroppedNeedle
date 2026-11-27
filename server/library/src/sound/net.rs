//! The model: MusiCNN trained on the Million Song Dataset (`MSD_musicnn`),
//! run one patch at a time on tract, a pure-Rust ONNX runtime, on one
//! thread.
//!
//! The two model files beside this crate (`server/library/models/`) are our
//! own export of the ISC-licensed checkpoint in
//! `github.com/jordipons/musicnn` at commit
//! `516acb2a0ff5ef73f64547898e018e793152c506` (TensorFlow CPU 2.21.0,
//! tf2onnx 1.17.0, opset 13), made and checked on 2026-11-27: on fixed
//! inputs they give AudioMuse-AI's published export to 1.9e-6. Their
//! licence travels with them (`MSD_musicnn-LICENSE`, and in the image
//! under `/usr/share/doc/droppedneedle/`). They are built into the binary,
//! never downloaded, and the loader checks each one's SHA-256:
//!
//! - `MSD_musicnn_embedding.onnx`, 3,127,526 bytes,
//!   `8db5a23bf9dfcbdb295ddbc14fe5e357b4ec63f419fb0030bd38ce60f616ac80`:
//!   a `[1, 187, 96]` patch to its 200 values at `model/dense/BiasAdd:0`,
//!   before the ReLU.
//! - `MSD_musicnn_head.onnx`, 42,900 bytes,
//!   `636812f247686ed06d687d6dacc05124868b4b5f0193cb4aada1385e8d44f64d`:
//!   those 200 values to the 50 tag logits.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tract_onnx::prelude::*;

use super::front::{N_MELS, PATCH_FRAMES};

/// The model's name, as rows and the status say it.
pub const MODEL: &str = "MSD_musicnn";
/// Values in a track vector.
pub const EMBEDDING: usize = 200;
/// Tag scores.
pub const TAG_COUNT: usize = 50;

/// The tags the head scores, in its output order (musicnn's
/// `MSD_LABELS`).
pub const TAGS: [&str; TAG_COUNT] = [
    "rock",
    "pop",
    "alternative",
    "indie",
    "electronic",
    "female vocalists",
    "dance",
    "00s",
    "alternative rock",
    "jazz",
    "beautiful",
    "metal",
    "chillout",
    "male vocalists",
    "classic rock",
    "soul",
    "indie rock",
    "Mellow",
    "electronica",
    "80s",
    "folk",
    "90s",
    "chill",
    "instrumental",
    "punk",
    "oldies",
    "blues",
    "hard rock",
    "ambient",
    "acoustic",
    "experimental",
    "female vocalist",
    "guitar",
    "Hip-Hop",
    "70s",
    "party",
    "country",
    "easy listening",
    "sexy",
    "catchy",
    "funk",
    "electro",
    "heavy metal",
    "Progressive rock",
    "60s",
    "rnb",
    "indie pop",
    "sad",
    "House",
    "happy",
];

const EMBEDDING_FILE: &[u8] = include_bytes!("../../models/MSD_musicnn_embedding.onnx");
const EMBEDDING_SHA256: &str = "8db5a23bf9dfcbdb295ddbc14fe5e357b4ec63f419fb0030bd38ce60f616ac80";
const HEAD_FILE: &[u8] = include_bytes!("../../models/MSD_musicnn_head.onnx");
const HEAD_SHA256: &str = "636812f247686ed06d687d6dacc05124868b4b5f0193cb4aada1385e8d44f64d";

/// The model could not be loaded or run.
#[derive(Debug, Clone, thiserror::Error)]
#[error("sound model: {0}")]
pub struct ModelError(pub String);

fn failed(error: impl std::fmt::Display) -> ModelError {
    ModelError(error.to_string())
}

type Plan = Arc<TypedRunnableModel>;

/// Both models, loaded and optimised for batches of one.
pub struct Models {
    embedding: Plan,
    head: Plan,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Models").finish_non_exhaustive()
    }
}

fn load(name: &str, bytes: &[u8], sha256: &str, shape: &[usize]) -> Result<Plan, ModelError> {
    let found = format!("{:x}", Sha256::digest(bytes));
    if found != sha256 {
        return Err(ModelError(format!(
            "{name} has SHA-256 {found}, not {sha256}"
        )));
    }
    tract_onnx::onnx()
        .model_for_read(&mut &bytes[..])
        .and_then(|model| model.with_input_fact(0, f32::fact(shape).into()))
        .and_then(|model| model.into_optimized())
        .and_then(|model| model.into_runnable())
        .map_err(|error| ModelError(format!("{name}: {error}")))
}

fn run(plan: &Plan, input: &[f32], shape: &[usize]) -> Result<Vec<f32>, ModelError> {
    let tensor = Tensor::from_shape(shape, input).map_err(failed)?;
    let outputs = plan.run(tvec!(tensor.into_tvalue())).map_err(failed)?;
    let output = outputs
        .first()
        .ok_or_else(|| ModelError("the model gave no output".to_owned()))?;
    Ok(output
        .to_plain_array_view::<f32>()
        .map_err(failed)?
        .iter()
        .copied()
        .collect())
}

impl Models {
    /// Load both models from the bytes built into the binary, checking
    /// each one's SHA-256 first. About 40 ms.
    pub fn load() -> Result<Self, ModelError> {
        Ok(Self {
            embedding: load(
                "MSD_musicnn_embedding.onnx",
                EMBEDDING_FILE,
                EMBEDDING_SHA256,
                &[1, PATCH_FRAMES, N_MELS],
            )?,
            head: load(
                "MSD_musicnn_head.onnx",
                HEAD_FILE,
                HEAD_SHA256,
                &[1, EMBEDDING],
            )?,
        })
    }

    /// One patch (187 frames of 96 bands) to its 200 values.
    pub fn embed(&self, patch: &[f32]) -> Result<Vec<f32>, ModelError> {
        let out = run(&self.embedding, patch, &[1, PATCH_FRAMES, N_MELS])?;
        if out.len() != EMBEDDING {
            return Err(ModelError(format!("{} values, not {EMBEDDING}", out.len())));
        }
        Ok(out)
    }

    /// A patch's 200 values to its 50 tag logits.
    pub fn tag_logits(&self, embedding: &[f32]) -> Result<Vec<f32>, ModelError> {
        let out = run(&self.head, embedding, &[1, EMBEDDING])?;
        if out.len() != TAG_COUNT {
            return Err(ModelError(format!("{} tags, not {TAG_COUNT}", out.len())));
        }
        Ok(out)
    }
}
