//! Why a track is on a station: one line built from the path that put it
//! there, and the names it is built from. No score or distance is part of
//! a line; a track found by sound carries its closeness word.

use droppedneedle_library::sound::Closeness;

use crate::discover::engine::reason::sound_line;
use crate::discover::models::{RadioReason, RadioReasonKind};

fn reason(kind: RadioReasonKind, line: String, seed_name: &str) -> RadioReason {
    RadioReason {
        kind,
        line,
        seed_name: seed_name.to_owned(),
        seed_track_id: None,
        closeness: None,
    }
}

/// By the station's own artist `name`, where it started.
pub fn seed(name: &str) -> RadioReason {
    let line = if name.is_empty() {
        "By the artist this station started from.".to_owned()
    } else {
        format!("By {name}, where this station started.")
    };
    reason(RadioReasonKind::Seed, line, name)
}

/// By an artist on the station's playlist, `playlist`.
pub fn on_playlist(playlist: &str) -> RadioReason {
    reason(
        RadioReasonKind::Seed,
        format!("By an artist on {playlist}."),
        playlist,
    )
}

/// From the shelf the station plays.
pub fn shelf() -> RadioReason {
    reason(
        RadioReasonKind::Shelf,
        "From the shelf you played.".to_owned(),
        "",
    )
}

/// By an artist tagged `genre`.
pub fn genre(genre: &str) -> RadioReason {
    reason(
        RadioReasonKind::Genre,
        format!("By an artist tagged {genre}."),
        genre,
    )
}

/// By `artist`, similar to the station's artist `seed_artist`.
pub fn close_to_artist(artist: &str, seed_artist: &str) -> RadioReason {
    let line = if seed_artist.is_empty() {
        format!("By {artist}, close to this station's artist.")
    } else {
        format!("By {artist}, close to {seed_artist}.")
    };
    reason(RadioReasonKind::CloseToArtist, line, seed_artist)
}

/// Found from `title`, a track the person asked for more like: by sound
/// when `track_id` is the track it sounds like, else by its artist.
pub fn more_like(title: &str, track_id: Option<&str>) -> RadioReason {
    RadioReason {
        seed_track_id: track_id.map(str::to_owned),
        ..reason(
            RadioReasonKind::MoreLike,
            format!("You asked for more like {title}."),
            title,
        )
    }
}

/// Sounds like the station's seed track `title` by `artist`, at
/// `distance`.
pub fn sounds_like(title: &str, artist: &str, track_id: &str, distance: f32) -> RadioReason {
    let closeness = Closeness::of(distance);
    RadioReason {
        seed_track_id: Some(track_id.to_owned()),
        closeness: Some(closeness),
        ..reason(
            RadioReasonKind::SoundsLike,
            sound_line(closeness, title, artist),
            title,
        )
    }
}
