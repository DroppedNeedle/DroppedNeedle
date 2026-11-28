//! By ear: the sound bench's section. It is on the page only while sound
//! analysis is on and has read a track, and is first tuned to the person's
//! most played library track of the last [`SEED_DAYS`] days that the
//! analysis has read, as the sound search would find it. With no such track
//! it is still shown, with no seed, and the page asks them to pick one.

use droppedneedle_library::sound::{MODEL, SoundError, SoundSearch};

use crate::discover::models::{BenchTrack, SectionContent, SoundBench};

/// How far back the first seed's plays count, in days.
pub const SEED_DAYS: i64 = 30;
/// A day, ms.
const DAY_MS: i64 = 86_400_000;

/// The bench for `user_id` at `now_ms`; `None` while sound analysis is off
/// or has read no track yet.
///
/// # Errors
///
/// A read's failure, or settings that cannot be read.
pub async fn section(
    sound: &SoundSearch,
    user_id: &str,
    now_ms: i64,
) -> Result<Option<SectionContent>, SoundError> {
    if !sound.enabled()? {
        return Ok(None);
    }
    let found = sound
        .bench_seed(user_id, now_ms - SEED_DAYS * DAY_MS)
        .await?;
    if !found.analysed {
        return Ok(None);
    }
    let seed = found.seed.map(|track| BenchTrack {
        track_id: track.track_id,
        title: track.title,
        artist_name: track.artist_name,
        album_id: track.album_id,
        album_title: track.album_title,
        duration_seconds: track.duration_seconds.map(|seconds| seconds.round() as i64),
    });
    Ok(Some(SectionContent::SoundBench {
        sound_bench: SoundBench {
            seed,
            model: MODEL.to_owned(),
            analysis_version: found.analysis_version,
        },
    }))
}
